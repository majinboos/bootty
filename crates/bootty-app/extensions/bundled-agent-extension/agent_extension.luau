--!strict
-- Bundled agent orchestration entrypoint.
-- The generic extension runtime supplies only bootty.commands/events plus optional generic host
-- primitives. All agent identity, adapter protocol, persistence, and UI policy stays in this package.
local core = require("core")
local model = require("model")
local ui = require("ui")
local installers = require("install")

local host = bootty or {}

local state, registration_error = core.register(host, { storage_key = "agents.records.v1", model = model, installers = installers })
if state == nil then
	error(registration_error and registration_error.error and registration_error.error.message or "agent extension registration failed")
end

local function records()
	local result = {}
	for _, agent_id in ipairs(state.order or {}) do
		local record = state.records and state.records[agent_id]
		if record ~= nil then result[#result + 1] = model.clone(record) end
	end
	return result
end

local function ingest(event)
	return core.handle(state, "agents.ingest", { event = event })
end

-- Publish package services through a namespaced table for generic UI/fixture consumers. These
-- helpers expose explicit normalized records only; they never inspect terminal/process output.
local agents = {
	state = state,
	model = model,
	ui = ui,
	installers = installers,
	describe = core.describe,
	records = records,
	ingest = ingest,
	handle = function(command_id, arguments)
		return core.handle(state, command_id, arguments)
	end,
	render = function(values, adapter)
		return ui.render(values or records(), adapter, host)
	end,
	manifests = function(provider)
		return core.handle(state, "agents.manifest.list", provider and { manifest_id = provider } or {})
	end,
}

host.agents = agents

-- The generic host may supply a subscription primitive. It is optional because event publishing is
-- still authoritative through host events; lack of a subscription is not treated as agent state.
if type(host.events) == "table" and type(host.events.subscribe) == "function" then
	pcall(host.events.subscribe, "agents.changed", function(event)
		if type(event) == "table" and event.records ~= nil then state.last_event = model.clone(event) end
	end)
end

return agents
