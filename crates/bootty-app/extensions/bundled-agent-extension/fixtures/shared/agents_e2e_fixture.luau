-- Shared deterministic e2e fixture for agents.list, agents.prompt, agents.changed, and cancellation.
-- It is a fake generic host: command/event calls are recorded, never sent to a process.

local fixture = {}

fixture.agent_id = "e2e/opaque/agent-01"
fixture.initial_records = {
	{
		agent_id = fixture.agent_id,
		provider = "codex",
		lifecycle = "idle",
		authority = { adapter_session_id = "native-opaque-01", sequence = 1 },
	},
}

local function copy(value, seen)
	if type(value) ~= "table" then
		return value
	end
	seen = seen or {}
	if seen[value] then
		return seen[value]
	end
	local result = {}
	seen[value] = result
	for key, child in pairs(value) do
		result[copy(key, seen)] = copy(child, seen)
	end
	return result
end

fixture.copy = copy

function fixture.new_host()
	local host = {
		calls = {},
		listeners = {},
		records = copy(fixture.initial_records),
	}
	host.commands = {}
	host.commands.invoke = function(command_id, arguments)
		host.calls[#host.calls + 1] = { id = command_id, args = copy(arguments or {}) }
		if command_id == "agents.list" then
			return { records = copy(host.records) }
		end
		if command_id == "agents.prompt" then
			return { task_id = "task-opaque-01", accepted = true }
		end
		if command_id == "agents.abort" then
			return { task_id = "task-opaque-01", cancelled = true }
		end
		return { command_id = command_id, accepted = true }
	end
	host.events = {}
	host.events.subscribe = function(topic, callback)
		host.listeners[topic] = host.listeners[topic] or {}
		host.listeners[topic][#host.listeners[topic] + 1] = callback
		return { topic = topic, subscription = "e2e-subscription" }
	end
	host.publish = function(topic, event)
		local callbacks = host.listeners[topic] or {}
		for _, callback in ipairs(callbacks) do
			callback(event)
		end
	end
	return host
end

return fixture
