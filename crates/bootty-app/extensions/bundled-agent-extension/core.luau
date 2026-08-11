--!strict
-- Bundled agent orchestration command core.
--
-- This module is deliberately host-neutral. It consumes explicit adapter records/events and uses
-- only generic command/event/storage/process/IPC/file primitives supplied by the host. It never
-- classifies process names, terminal text, screen state, or transcript tails as agent facts.
local M = {}

local COMMANDS = {
	{ id = "agents.list", title = "List Agents", description = "List normalized agent records.", aliases = { "agent.list" }, mutation = "read", arguments = { { name = "provider", type = "string" }, { name = "state", type = "string" } } },
	{ id = "agents.get", title = "Get Agent", description = "Get one normalized agent record.", aliases = { "agent.get" }, mutation = "read", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.read", title = "Read Agent", description = "Read bounded authoritative agent output.", aliases = { "agent.read" }, mutation = "read", arguments = { { name = "agent_id", type = "string", required = true }, { name = "stream", type = "string" }, { name = "limit", type = "integer", minimum = 1 } } },
	{ id = "agents.explain", title = "Explain Agent", description = "Explain the authoritative facts behind an agent record.", aliases = { "agent.explain" }, mutation = "read", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.start", title = "Start Agent", description = "Start an agent through a configured adapter.", aliases = { "agent.start", "claude" }, mutation = "write", arguments = { { name = "provider", type = "string", required = true }, { name = "cwd", type = "string" }, { name = "argv", type = "array" }, { name = "environment", type = "object" }, { name = "prompt", type = "string" }, { name = "managed", type = "boolean" } } },
	{ id = "agents.focus", title = "Focus Agent", description = "Focus the terminal associated with an agent.", aliases = { "agent.focus" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.rename", title = "Rename Agent", description = "Set an explicit agent display name.", aliases = { "agent.rename" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true }, { name = "name", type = "string", required = true } } },
	{ id = "agents.prompt", title = "Prompt Agent", description = "Prompt an agent through its authoritative adapter.", aliases = { "agent.prompt" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true }, { name = "prompt", type = "string", required = true }, { name = "mode", type = "string" } } },
	{ id = "agents.send_text", title = "Send Text", description = "Send text through an agent terminal or protocol.", aliases = { "agent.send_text" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true }, { name = "text", type = "string", required = true } } },
	{ id = "agents.send_keys", title = "Send Keys", description = "Send keys through an agent terminal or protocol.", aliases = { "agent.send_keys" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true }, { name = "keys", type = "array", required = true } } },
	{ id = "agents.wait", title = "Wait Agent", description = "Wait for an authoritative lifecycle transition.", aliases = { "agent.wait" }, mutation = "read", arguments = { { name = "agent_id", type = "string", required = true }, { name = "timeout_ms", type = "integer", minimum = 0 } } },
	{ id = "agents.pause", title = "Pause Agent", description = "Pause an agent when its adapter supports pause.", aliases = { "agent.pause" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.resume", title = "Resume Agent", description = "Resume an agent when its adapter supports resume.", aliases = { "agent.resume" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.retry", title = "Retry Agent", description = "Retry an agent operation when its adapter supports retry.", aliases = { "agent.retry" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.abort", title = "Abort Agent", description = "Abort an agent through its authoritative adapter.", aliases = { "agent.abort" }, mutation = "destructive", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.view.set", title = "Set Agent View", description = "Set generic view metadata for an agent.", aliases = { "agent.view.set" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true }, { name = "surface", type = "object", required = true } } },
	{ id = "agents.view.clear", title = "Clear Agent View", description = "Clear generic view metadata.", aliases = { "agent.view.clear" }, mutation = "write", arguments = { { name = "agent_id", type = "string" } } },
	{ id = "agents.report", title = "Report Agent", description = "Report one normalized agent record.", aliases = { "pane.report_agent", "agent.report" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.report_session", title = "Report Agent Session", description = "Report a native adapter session by opaque ID.", aliases = { "pane.report_agent_session", "agent.report_session" }, mutation = "write", arguments = { { name = "provider", type = "string", required = true }, { name = "native_session_id", type = "string", required = true } } },
	{ id = "agents.clear_authority", title = "Clear Agent Authority", description = "Clear explicit authority for one agent.", aliases = { "pane.clear_agent_authority" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.release", title = "Release Agent", description = "Release an agent record explicitly.", aliases = { "pane.release_agent" }, mutation = "write", arguments = { { name = "agent_id", type = "string", required = true } } },
	{ id = "agents.manifest.list", title = "List Agent Manifests", description = "List bundled adapter manifests.", aliases = { "server.agent_manifests" }, mutation = "read", arguments = {} },
	{ id = "agents.manifest.reload", title = "Reload Agent Manifests", description = "Reload bundled adapter manifest metadata.", aliases = { "server.reload_agent_manifests" }, mutation = "write", arguments = { { name = "manifest_id", type = "string" } } },
	{ id = "agents.integration.list", title = "List Integrations", description = "List bundled adapter integrations.", aliases = { "agent.integration.list" }, mutation = "read", arguments = { { name = "provider", type = "string" } } },
	{ id = "agents.integration.install", title = "Install Integration", description = "Preview or install one adapter integration.", aliases = { "integration.install" }, mutation = "write", arguments = { { name = "provider", type = "string", required = true } } },
	{ id = "agents.integration.uninstall", title = "Uninstall Integration", description = "Preview or uninstall one adapter integration.", aliases = { "integration.uninstall" }, mutation = "destructive", arguments = { { name = "provider", type = "string", required = true } } },
	{ id = "agents.integration.setup", title = "Setup Integration", description = "Preview or set up one adapter integration.", aliases = { "agent.integration.setup" }, mutation = "write", arguments = { { name = "provider", type = "string", required = true } } },
	{ id = "agents.integration.doctor", title = "Doctor Integration", description = "Diagnose one adapter integration.", aliases = { "agent.integration.doctor" }, mutation = "read", arguments = { { name = "provider", type = "string" } } },
	{ id = "agents.ingest", title = "Ingest Agents", description = "Receive authoritative native adapter events.", aliases = { "agent.ingest" }, mutation = "write", arguments = { { name = "event", type = "object", required = true }, { name = "adapter", type = "string" }, { name = "event_name", type = "string" } } },
}

local RESULT_SCHEMAS = {
	["agents.list"] = { type = "array", items = { type = "object" } },
	["agents.manifest.list"] = { type = "array", items = { type = "object" } },
	["agents.integration.list"] = { type = "array", items = { type = "object" } },
	["agents.wait"] = { type = "object", properties = { task_id = { type = "string" } } },
}
for _, descriptor in ipairs(COMMANDS) do
	descriptor.args = descriptor.arguments
	descriptor.result_schema = RESULT_SCHEMAS[descriptor.id] or { type = "object" }
end

local ALIASES = {}
for _, descriptor in ipairs(COMMANDS) do
	for _, alias in ipairs(descriptor.aliases or {}) do
		ALIASES[alias] = descriptor.id
	end
end

local function is_table(value)
	return type(value) == "table"
end

local function copy(value, seen)
	if not is_table(value) then
		return value
	end
	seen = seen or {}
	if seen[value] ~= nil then
		return seen[value]
	end
	local result = {}
	seen[value] = result
	for key, child in pairs(value) do
		result[copy(key, seen)] = copy(child, seen)
	end
	return result
end

local function is_array(value)
	if not is_table(value) then
		return false
	end
	local max = 0
	local count = 0
	for key in pairs(value) do
		if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
			return false
		end
		count = count + 1
		if key > max then max = key end
	end
	return count == max
end

-- Small JSON decoder for generic runtime argument strings. Decoding is data-only; no Lua code is
-- evaluated. Hosts that pass decoded objects bypass this routine.
local function decode_json(text)
	if type(text) ~= "string" then return nil end
	local index = 1
	local length = #text
	local parse_value
	local function skip_space()
		while index <= length do
			local c = text:sub(index, index)
			if c == " " or c == "\t" or c == "\n" or c == "\r" then index = index + 1 else break end
		end
	end
	local function parse_string()
		if text:sub(index, index) ~= '"' then return nil end
		index = index + 1
		local out = {}
		while index <= length do
			local c = text:sub(index, index)
			index = index + 1
			if c == '"' then return table.concat(out) end
			if c == "\\" then
				local e = text:sub(index, index)
				index = index + 1
				local escapes = { ['"'] = '"', ["\\"] = "\\", ["/"] = "/", b = "\b", f = "\f", n = "\n", r = "\r", t = "\t" }
				if escapes[e] ~= nil then
					out[#out + 1] = escapes[e]
				elseif e == "u" then
					local hex = text:sub(index, index + 3)
					if #hex ~= 4 or not hex:match("^[0-9a-fA-F]+$") then return nil end
					local code = tonumber(hex, 16)
					if code < 128 then out[#out + 1] = string.char(code) else out[#out + 1] = "?" end
					index = index + 4
				else return nil end
			else
				out[#out + 1] = c
			end
		end
		return nil
	end
	local function parse_number()
		local start = index
		if text:sub(index, index) == "-" then index = index + 1 end
		while text:sub(index, index):match("%d") do index = index + 1 end
		if text:sub(index, index) == "." then
			index = index + 1
			while text:sub(index, index):match("%d") do index = index + 1 end
		end
		local e = text:sub(index, index)
		if e == "e" or e == "E" then
			index = index + 1
			local sign = text:sub(index, index)
			if sign == "+" or sign == "-" then index = index + 1 end
			while text:sub(index, index):match("%d") do index = index + 1 end
		end
		return tonumber(text:sub(start, index - 1))
	end
	local function parse_array()
		index = index + 1
		local result = {}
		skip_space()
		if text:sub(index, index) == "]" then index = index + 1; return result end
		while index <= length do
			local item = parse_value()
			if item == nil and text:sub(index, index) ~= "n" then return nil end
			result[#result + 1] = item
			skip_space()
			local separator = text:sub(index, index)
			if separator == "]" then index = index + 1; return result end
			if separator ~= "," then return nil end
			index = index + 1
			skip_space()
		end
		return nil
	end
	local function parse_object()
		index = index + 1
		local result = {}
		skip_space()
		if text:sub(index, index) == "}" then index = index + 1; return result end
		while index <= length do
			local key = parse_string()
			if key == nil then return nil end
			skip_space()
			if text:sub(index, index) ~= ":" then return nil end
			index = index + 1
			skip_space()
			local value = parse_value()
			result[key] = value
			skip_space()
			local separator = text:sub(index, index)
			if separator == "}" then index = index + 1; return result end
			if separator ~= "," then return nil end
			index = index + 1
			skip_space()
		end
		return nil
	end
	parse_value = function()
		skip_space()
		local c = text:sub(index, index)
		if c == '"' then return parse_string() end
		if c == "{" then return parse_object() end
		if c == "[" then return parse_array() end
		if text:sub(index, index + 3) == "true" then index = index + 4; return true end
		if text:sub(index, index + 4) == "false" then index = index + 5; return false end
		if text:sub(index, index + 3) == "null" then index = index + 4; return nil end
		return parse_number()
	end
	local result = parse_value()
	skip_space()
	if index <= length then return nil end
	return result
end

local function descriptor_for(id)
	local canonical = ALIASES[id] or id
	for _, descriptor in ipairs(COMMANDS) do
		if descriptor.id == canonical then return descriptor end
	end
	return nil
end

local function normalize_args(id, raw)
	if is_table(raw) and not is_array(raw) then return copy(raw) end
	local values = {}
	if is_table(raw) then
		for index = 1, #raw do
			local value = raw[index]
			local decoded = decode_json(value)
			values[index] = decoded ~= nil and decoded or value
		end
	else
		values[1] = raw
	end
	if #values == 1 and is_table(values[1]) then return values[1] end
	local descriptor = descriptor_for(id)
	local result = {}
	if descriptor ~= nil then
		for index, argument in ipairs(descriptor.arguments or {}) do
			if values[index] ~= nil then result[argument.name] = values[index] end
		end
	end
	return result
end

local function result_error(code, message, details)
	local result = { ok = false, code = code, message = message, error = { code = code, message = message } }
	if is_table(details) then
		for key, value in pairs(details) do
			result[key] = copy(value)
			result.error[key] = copy(value)
		end
	end
	return result
end

local function unsupported(command, agent, message)
	local details = { command = command, authority = "adapter" }
	if is_table(agent) then
		details.agent_id = agent.agent_id
		details.provider = agent.provider
	end
	return result_error("unsupported", message or "adapter does not expose this control", details)
end

local function safe_call(object, method, ...)
	if not is_table(object) or type(object[method]) ~= "function" then return false, nil end
	local ok, first, second = pcall(object[method], ...)
	if ok then return true, first, second end
	local retry_ok, retry_first, retry_second = pcall(object[method], object, ...)
	if retry_ok then return true, retry_first, retry_second end
	return false, first
end

local function load_module(name, paths)
	local ok, value = pcall(require, name)
	if ok then return value end
	if type(loadfile) == "function" then
		for _, path in ipairs(paths or {}) do
			local chunk = loadfile(path)
			if chunk ~= nil then
				local loaded, result = pcall(chunk)
				if loaded then return result end
			end
		end
	end
	return nil
end

local function adapter_for(state, provider)
	if type(provider) ~= "string" or provider == "" then return nil end
	if state.adapters[provider] ~= nil then return state.adapters[provider] end
	local adapter = load_module("adapters." .. provider, {
		"./adapters/" .. provider .. "/adapter.luau",
		"adapters/" .. provider .. "/adapter.luau",
	})
	state.adapters[provider] = adapter or false
	return adapter
end

local ADAPTER_MANIFESTS = {
	pi = { provider = "pi", name = "Pi", version = "0.84.1", existing = "typescript_extension", managed = "stdio_jsonl_rpc" },
	claude = { provider = "claude", name = "Claude Code", version = "2.0", existing = "hooks", managed = "stream-json" },
	codex = { provider = "codex", name = "OpenAI Codex CLI", version = "0.1", existing = "hooks", managed = "app-server" },
	opencode = { provider = "opencode", name = "OpenCode", version = "1.18.16", existing = "plugin", managed = "server_sse_sdk" },
}

local function manifest_list(provider)
	local result = {}
	if provider ~= nil and ADAPTER_MANIFESTS[provider] ~= nil then
		result[1] = copy(ADAPTER_MANIFESTS[provider])
		return result
	end
	for _, id in ipairs({ "pi", "claude", "codex", "opencode" }) do result[#result + 1] = copy(ADAPTER_MANIFESTS[id]) end
	return result
end

local function storage_for(host)
	if is_table(host) and is_table(host.storage) then return host.storage end
	return nil
end

local function storage_read(storage, key)
	if storage == nil then return nil end
	for _, method in ipairs({ "get", "read", "load" }) do
		local ok, value = safe_call(storage, method, key)
		if ok and value ~= nil then
			if type(value) == "string" then return decode_json(value) or value end
			return value
		end
	end
	return nil
end

local function storage_write(storage, key, value)
	if storage == nil then return false end
	for _, method in ipairs({ "set", "write", "save" }) do
		local ok = safe_call(storage, method, key, copy(value))
		if ok then return true end
	end
	return false
end

local function records_array(state)
	local result = {}
	for _, agent_id in ipairs(state.order) do
		local record = state.records[agent_id]
		if record ~= nil then result[#result + 1] = copy(record) end
	end
	return result
end

local function save_state(state)
	local snapshot = { records = records_array(state), revision = state.revision }
	storage_write(storage_for(state.host), state.storage_key, snapshot)
end

local function publish_changed(state, outcome)
	local payload = { topic = "agents.changed", revision = state.revision, outcome = copy(outcome), records = records_array(state) }
	local events = is_table(state.host) and state.host.events or nil
	if events ~= nil then
		local ok = safe_call(events, "publish", "agents.changed", payload)
		if not ok then safe_call(events, "emit", "agents.changed", payload) end
	end
	return payload
end

local function replace_record(state, record)
	if not is_table(record) or type(record.agent_id) ~= "string" or record.agent_id == "" then return false end
	local id = record.agent_id
	if state.records[id] == nil then state.order[#state.order + 1] = id end
	state.records[id] = copy(record)
	state.revision = state.revision + 1
	save_state(state)
	return true
end

local function remove_record(state, agent_id)
	if type(agent_id) ~= "string" or state.records[agent_id] == nil then return false end
	state.records[agent_id] = nil
	for index, id in ipairs(state.order) do
		if id == agent_id then table.remove(state.order, index); break end
	end
	state.revision = state.revision + 1
	save_state(state)
	return true
end

local function state_new(host, options)
	options = options or {}
	local state = {
		host = host or {},
		revision = 0,
		records = {},
		order = {},
		events = {},
		adapters = options.adapters or {},
		model = options.model,
		installers = options.installers,
		storage_key = options.storage_key or "agents.records.v1",
	}
	local persisted = storage_read(storage_for(state.host), state.storage_key)
	if is_table(persisted) and is_table(persisted.records) then
		for _, record in ipairs(persisted.records) do replace_record(state, record) end
		state.revision = tonumber(persisted.revision) or state.revision
	end
	return state
end

M.state = state_new
M.new_state = state_new
M.commands = COMMANDS
M.descriptors = COMMANDS

local function command_list(state, args)
	local result = {}
	for _, id in ipairs(state.order) do
		local record = state.records[id]
		if record ~= nil and (args.provider == nil or record.provider == args.provider) and (args.state == nil or (record.lifecycle or record.status) == args.state) then
			result[#result + 1] = copy(record)
		end
	end
	return result
end

local function require_agent(state, args)
	local id = args.agent_id
	if type(id) ~= "string" or id == "" then return nil, result_error("invalid_argument", "agent_id is required", { argument = "agent_id" }) end
	local record = state.records[id]
	if record == nil then return nil, result_error("not_found", "agent record was not found", { agent_id = id }) end
	return record, nil
end

local function adapter_control(state, command, record, args, method)
	local adapter = adapter_for(state, record.provider)
	if adapter ~= nil and type(adapter[method]) == "function" then
		local ok, value, detail = pcall(adapter[method], args, record)
		if not ok then return result_error("adapter_failed", tostring(value), { provider = record.provider, agent_id = record.agent_id }) end
		if value ~= nil then return value end
		return { accepted = true, command_id = command, agent_id = record.agent_id, provider = record.provider }
	end
	local ipc = is_table(state.host) and (state.host.ipc or state.host.process) or nil
	if ipc ~= nil then
		local ok, value = safe_call(ipc, "invoke", record.provider, method, copy(args))
		if ok then return value end
	end
	return unsupported(command, record, "adapter control is unavailable")
end

local function handle_command(state, command_id, raw_args)
	local id = ALIASES[command_id] or command_id
	local args = normalize_args(id, raw_args)
	if id == "agents.list" then return command_list(state, args) end
	if id == "agents.manifest.list" then return manifest_list() end
	if id == "agents.manifest.reload" then return { reloaded = true, manifest_id = args.manifest_id, manifests = manifest_list(args.manifest_id) } end
	if id == "agents.integration.list" then return manifest_list(args.provider) end
	if id == "agents.integration.install" or id == "agents.integration.setup" or id == "agents.integration.uninstall" or id == "agents.integration.doctor" then
		local provider = args.provider
		if type(provider) ~= "string" or provider == "" then return result_error("invalid_argument", "provider is required", { argument = "provider" }) end
		local installer = state.installers or load_module("install", { "./install.luau", "install.luau" })
		if installer ~= nil then
			local method = ({ ["agents.integration.install"] = "integration_install", ["agents.integration.setup"] = "integration_setup", ["agents.integration.uninstall"] = "integration_uninstall", ["agents.integration.doctor"] = "integration_doctor" })[id]
			if type(installer[method]) == "function" then
				local ok, value = pcall(installer[method], provider, args)
				if ok then return value end
				return result_error("installer_failed", tostring(value), { provider = provider })
			end
		end
		return result_error("unsupported", "installer workflow is unavailable", { provider = provider })
	end
	if id == "agents.ingest" then
		local raw_event = copy(args.event)
		if not is_table(raw_event) then return result_error("invalid_argument", "event must be an object", { argument = "event" }) end
		local event = copy(raw_event)
		local requested_adapter = args.adapter
		if requested_adapter ~= nil and (type(requested_adapter) ~= "string" or requested_adapter == "") then
			return result_error("invalid_argument", "adapter must be a non-empty string", { argument = "adapter" })
		end
		if requested_adapter ~= nil and requested_adapter ~= "pi" and requested_adapter ~= "claude" and requested_adapter ~= "codex" and requested_adapter ~= "opencode" then
			return result_error("unsupported_adapter", "adapter must be pi, claude, codex, or opencode", { adapter = requested_adapter })
		end
		local existing_adapter = event.adapter or event.provider
		if existing_adapter == nil and (event.source == "pi" or event.source == "claude" or event.source == "codex" or event.source == "opencode") then
			existing_adapter = event.source
		end
		if existing_adapter ~= nil and (type(existing_adapter) ~= "string" or existing_adapter == "") then
			return result_error("invalid_argument", "event adapter must be a non-empty string", { argument = "event.adapter" })
		end
		if existing_adapter ~= nil and existing_adapter ~= "pi" and existing_adapter ~= "claude" and existing_adapter ~= "codex" and existing_adapter ~= "opencode" then
			return result_error("unsupported_adapter", "event adapter must be pi, claude, codex, or opencode", { adapter = existing_adapter })
		end
		if requested_adapter ~= nil and existing_adapter ~= nil and existing_adapter ~= requested_adapter then
			return result_error("conflicting_argument", "adapter conflicts with the authoritative event source", { adapter = requested_adapter, source = existing_adapter })
		end
		if requested_adapter ~= nil and existing_adapter == nil then event.adapter = requested_adapter end
		local event_name = args.event_name
		if event_name ~= nil and (type(event_name) ~= "string" or event_name == "") then
			return result_error("invalid_argument", "event_name must be a non-empty string", { argument = "event_name" })
		end
		local existing_name = event.type or event.event or event.event_name or event.hook_event
		if existing_name ~= nil and (type(existing_name) ~= "string" or existing_name == "") then
			return result_error("invalid_argument", "event kind must be a non-empty string", { argument = "event_name" })
		end
		if event_name ~= nil and existing_name ~= nil and existing_name ~= event_name then
			return result_error("conflicting_argument", "event_name conflicts with the authoritative event kind", { event_name = event_name, event_type = existing_name })
		end
		if event_name ~= nil and existing_name == nil then event.event_name = event_name end
		if event.raw == nil then event.raw = raw_event end
		local model = state.model
		if model == nil then
			local ok, loaded = pcall(function() return load_module("model", { "./model.luau", "model.luau" }) end)
			if ok then model = loaded end
			state.model = model
		end
		if model == nil or type(model.apply_event) ~= "function" then return result_error("runtime_unavailable", "normalized model is unavailable") end
		state.model_store = state.model_store or model.new_store()
		local outcome = model.apply_event(state.model_store, event)
		if outcome.record ~= nil then replace_record(state, outcome.record) end
		for _, record in ipairs(outcome.records or {}) do replace_record(state, record) end
		if outcome.changed then return { accepted = true, changed = true, status = outcome.status, event = outcome.event, records = records_array(state), revision = state.revision, published = publish_changed(state, outcome) } end
		return { accepted = true, changed = false, status = outcome.status, event = outcome.event, records = records_array(state), revision = state.revision }
	end
	if id == "agents.get" or id == "agents.read" or id == "agents.explain" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		if id == "agents.explain" then return { agent_id = record.agent_id, provider = record.provider, lifecycle = record.lifecycle, authority = copy(record.authority), facts = copy(record.facts or record) } end
		if id == "agents.read" then
			return adapter_control(state, id, record, args, "read")
		end
		return { agent_id = record.agent_id, record = copy(record) }
	end
	if id == "agents.report" then
		local agent_id = args.agent_id
		if type(agent_id) ~= "string" or agent_id == "" then return result_error("invalid_argument", "agent_id is required", { argument = "agent_id" }) end
		local record = state.records[agent_id]
		if record == nil then
			record = { agent_id = agent_id }
			if args.facts ~= nil then record.facts = copy(args.facts) end
			replace_record(state, record)
		end
		return { agent_id = agent_id, record = copy(record), reported = true, published = publish_changed(state, { kind = "report", record = record }) }
	end
	if id == "agents.report_session" then
		local provider, native_id = args.provider, args.native_session_id
		if type(provider) ~= "string" or provider == "" or type(native_id) ~= "string" or native_id == "" then
			return result_error("invalid_argument", "provider and native_session_id are required")
		end
		for _, agent_id in ipairs(state.order) do
			local record = state.records[agent_id]
			local authority = record and record.authority
			local native = record and (record.native_session_id or record.adapter_session_id or (authority and (authority.native_session_id or authority.adapter_session_id)))
			if record ~= nil and record.provider == provider and native == native_id then return { agent_id = record.agent_id, record = copy(record), reported = true } end
		end
		local adapter = adapter_for(state, provider)
		if adapter ~= nil and type(adapter.report_session) == "function" then
			local ok, value = pcall(adapter.report_session, { provider = provider, native_session_id = native_id })
			if ok and value ~= nil then return value end
		end
		return { accepted = true, pending = true, provider = provider, native_session_id = native_id, diagnostic = { code = "awaiting_authoritative_session", message = "the adapter has not published an agent identity for this opaque session" } }
	end
	if id == "agents.start" then
		if type(args.provider) ~= "string" or args.provider == "" then return result_error("invalid_argument", "provider is required", { argument = "provider" }) end
		local adapter = adapter_for(state, args.provider)
		if adapter ~= nil and type(adapter.start) == "function" then
			local ok, value = pcall(adapter.start, args)
			if ok and value ~= nil then return value end
		end
		return { accepted = false, pending = true, provider = args.provider, managed = args.managed == true, diagnostic = { code = "awaiting_authoritative_session", message = "agent identity will be published by the adapter; no identity was inferred" } }
	end
	if id == "agents.focus" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local sessions = is_table(state.host) and state.host.sessions or nil
		if sessions ~= nil then
			local ok, value = safe_call(sessions, "focus", record.terminal_id or (record.terminal and record.terminal.terminal_id), copy(record))
			if ok then return value end
		end
		return adapter_control(state, id, record, args, "focus")
	end
	if id == "agents.rename" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		record = copy(record); record.name = args.name; replace_record(state, record)
		return { changed = true, agent_id = record.agent_id, record = copy(record), published = publish_changed(state, { kind = "rename", record = record }) }
	end
	if id == "agents.prompt" or id == "agents.send_text" or id == "agents.send_keys" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local method = ({ ["agents.prompt"] = "prompt", ["agents.send_text"] = "send_text", ["agents.send_keys"] = "send_keys" })[id]
		return adapter_control(state, id, record, args, method)
	end
	if id == "agents.wait" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		return { agent_id = record.agent_id, status = record.lifecycle or record.status or "unknown", completed = record.lifecycle == "completed" or record.lifecycle == "failed" or record.lifecycle == "aborted", timeout_ms = args.timeout_ms }
	end
	local control_states = { ["agents.pause"] = "paused", ["agents.resume"] = "running", ["agents.retry"] = "retrying", ["agents.abort"] = "aborted" }
	if control_states[id] ~= nil then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local method = id:sub(8)
		local result = adapter_control(state, id, record, args, method)
		if not (is_table(result) and result.ok == false) then
			local updated = copy(record); updated.lifecycle = control_states[id]; replace_record(state, updated)
			if is_table(result) then result.agent_id = record.agent_id; result.record = copy(updated); result.published = publish_changed(state, { kind = id, record = updated }) end
		end
		return result
	end
	if id == "agents.view.set" or id == "agents.view.clear" or id == "agents.clear_authority" then
		if id == "agents.view.clear" and args.agent_id == nil then
			local changed = false
			for _, agent_id in ipairs(state.order) do
				local record = state.records[agent_id]
				if record ~= nil and record.view ~= nil then
					local updated = copy(record); updated.view = nil; replace_record(state, updated); changed = true
				end
			end
			return { changed = changed, cleared = true, published = changed and publish_changed(state, { kind = id }) or nil }
		end
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local updated = copy(record)
		if id == "agents.view.set" then updated.view = copy(args.surface) elseif id == "agents.view.clear" then updated.view = nil else updated.authority = nil end
		replace_record(state, updated)
		return { changed = true, agent_id = updated.agent_id, record = copy(updated), published = publish_changed(state, { kind = id, record = updated }) }
	end
	if id == "agents.release" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local updated = copy(record)
		updated.lifecycle = "released"
		replace_record(state, updated)
		return { changed = true, released = true, agent_id = updated.agent_id, record = copy(updated), published = publish_changed(state, { kind = id, record = updated }) }
	end
	return result_error("unknown_command", "agent command is not registered", { command = command_id })
end

M.handle = handle_command
M.invoke = handle_command

function M.describe()
	return copy(COMMANDS)
end

function M.register(host, options)
	options = options or {}
	host = host or (type(bootty) == "table" and bootty) or {}
	local state = options.state or state_new(host, options)
	local commands = host.commands
	if not is_table(commands) or type(commands.register) ~= "function" then return nil, result_error("runtime_unavailable", "generic command registration is unavailable") end
	for _, source_descriptor in ipairs(COMMANDS) do
		local descriptor = copy(source_descriptor)
		descriptor.handler = function(arguments)
			return handle_command(state, descriptor.id, arguments)
		end
		local ok = safe_call(commands, "register", descriptor)
		if not ok then return nil, result_error("registration_failed", "failed to register agent command", { command = descriptor.id }) end
	end
	local events = host.events
	if is_table(events) and type(events.register) == "function" then safe_call(events, "register", "agents.changed") end
	return state
end

M.register_commands = M.register
return M
