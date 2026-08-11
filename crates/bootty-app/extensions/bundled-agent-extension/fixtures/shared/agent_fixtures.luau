-- Deterministic normalized records/events shared by the bundled-agent fixture tests.
-- The generic runner loads this file once and passes its returned table as `fixtures`.

local fixture = {}

fixture.command_ids = {
	"agents.abort",
	"agents.clear_authority",
	"agents.explain",
	"agents.focus",
	"agents.get",
	"agents.ingest",
	"agents.integration.doctor",
	"agents.integration.install",
	"agents.integration.list",
	"agents.integration.setup",
	"agents.integration.uninstall",
	"agents.list",
	"agents.manifest.list",
	"agents.manifest.reload",
	"agents.pause",
	"agents.prompt",
	"agents.read",
	"agents.release",
	"agents.rename",
	"agents.report",
	"agents.report_session",
	"agents.resume",
	"agents.retry",
	"agents.send_keys",
	"agents.send_text",
	"agents.start",
	"agents.view.clear",
	"agents.view.set",
	"agents.wait",
}

fixture.records = {
	{
		agent_id = "opaque/pi/session/01",
		provider = "pi",
		lifecycle = "idle",
		authority = { adapter_session_id = "pi-native-opaque", sequence = 1 },
		native_ids = { session = "pi-native-opaque" },
		updated_at = "2026-08-11T00:00:01Z",
		evidence = { { kind = "adapter", source = "pi", sequence = 1 } },
	},
	{
		agent_id = "opaque/claude:alpha",
		provider = "claude",
		lifecycle = "waiting",
		authority = { adapter_session_id = "claude-native-opaque", sequence = 2 },
		prompt = "approve the change",
		permission = { kind = "approval", state = "requested" },
		updated_at = "2026-08-11T00:00:02Z",
	},
	{
		agent_id = "opaque/codex?beta",
		provider = "codex",
		lifecycle = "running",
		terminal = { terminal_id = "terminal-opaque", process_id = "process-opaque" },
		model = "gpt-5",
		usage = { input = 10, output = 4 },
		updated_at = "2026-08-11T00:00:03Z",
	},
	{
		agent_id = "opaque/opencode#gamma",
		provider = "opencode",
		lifecycle = "unknown",
		fidelity = { level = "partial", fields = { "provider" } },
		updated_at = "2026-08-11T00:00:04Z",
	},
}

fixture.lifecycle_events = {
	{ event_id = "evt-3", sequence = 3, record = { agent_id = "opaque/pi/session/01", lifecycle = "completed" } },
	{ event_id = "evt-1", sequence = 1, record = { agent_id = "opaque/pi/session/01", lifecycle = "running" } },
	{ event_id = "evt-1", sequence = 1, record = { agent_id = "opaque/pi/session/01", lifecycle = "running" } },
	{ event_id = "evt-2", sequence = 2, record = { agent_id = "opaque/pi/session/01", lifecycle = "waiting" } },
	{ event_id = "evt-remove", sequence = 4, operation = "remove", agent_id = "opaque/opencode#gamma" },
}

fixture.rebase_event = {
	event_id = "evt-rebase",
	sequence = 8,
	rebase = true,
	snapshot = {
		records = {
			{ agent_id = "opaque/pi/session/01", lifecycle = "idle", authority = { sequence = 8 } },
			{ agent_id = "opaque/new-agent/9", provider = "claude", lifecycle = "starting" },
		},
	},
}

fixture.adapters = {
	pi = {
		provider = "pi",
		native_session_id = "pi-native-opaque",
		facts = { lifecycle = "running", model = "pi-model" },
	},
	claude = {
		provider = "claude",
		native_session_id = "claude-native-opaque",
		facts = { lifecycle = "running", permission = { kind = "approval" } },
	},
	codex = {
		provider = "codex",
		native_session_id = "codex-native-opaque",
		facts = { lifecycle = "running", usage = { input = 1, output = 2 } },
	},
	opencode = {
		provider = "opencode",
		native_session_id = "opencode-native-opaque",
		facts = { lifecycle = "running", tool = "shell" },
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
		state = { records = copy(fixture.records) },
	}
	host.commands = {}
	host.commands.invoke = function(command_id, arguments)
		host.calls[#host.calls + 1] = { id = command_id, args = copy(arguments or {}) }
		if command_id == "agents.list" then
			return { records = copy(host.state.records) }
		end
		return { command_id = command_id, accepted = true, args = copy(arguments or {}) }
	end
	host.events = {}
	host.events.subscribe = function(topic, callback)
		host.listeners[topic] = host.listeners[topic] or {}
		host.listeners[topic][#host.listeners[topic] + 1] = callback
		return { topic = topic, subscription = "fixture-" .. tostring(#host.listeners[topic]) }
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
