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
local function encode_json(value, seen)
	local value_type = type(value)
	if value == nil then return "null" end
	if value_type == "boolean" then return value and "true" or "false" end
	if value_type == "number" then
		if value ~= value or value == math.huge or value == -math.huge then error("non-finite JSON number") end
		return tostring(value)
	end
	if value_type == "string" then
		return '"' .. value:gsub('[%z\1-\31\\"]', function(character)
			local escapes = { ['"'] = '\\"', ["\\"] = "\\\\", ["\b"] = "\\b", ["\f"] = "\\f", ["\n"] = "\\n", ["\r"] = "\\r", ["\t"] = "\\t" }
			return escapes[character] or string.format("\\u%04x", string.byte(character))
		end) .. '"'
	end
	if value_type ~= "table" then error("unsupported JSON value") end
	seen = seen or {}
	if seen[value] then error("cyclic JSON value") end
	seen[value] = true
	local array = is_array(value)
	local parts = {}
	if array then
		for index = 1, #value do parts[#parts + 1] = encode_json(value[index], seen) end
		seen[value] = nil
		return "[" .. table.concat(parts, ",") .. "]"
	end
	local keys = {}
	for key in pairs(value) do keys[#keys + 1] = key end
	table.sort(keys, function(left, right) return tostring(left) < tostring(right) end)
	for _, key in ipairs(keys) do
		parts[#parts + 1] = encode_json(tostring(key), seen) .. ":" .. encode_json(value[key], seen)
	end
	seen[value] = nil
	return "{" .. table.concat(parts, ",") .. "}"
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
	local candidates = { name }
	if type(name) == "string" then
		candidates[#candidates + 1] = name:gsub("%.", "/")
	end
	for _, candidate in ipairs(candidates) do
		local ok, value = pcall(require, candidate)
		if ok and value ~= nil then return value end
	end
	if type(loadfile) == "function" then
		for _, path in ipairs(paths or {}) do
			local chunk = loadfile(path)
			if chunk ~= nil then
				local loaded, result = pcall(chunk)
				if loaded and result ~= nil then return result end
			end
		end
	end
	return nil
end

local ADAPTER_IDS = { pi = true, claude = true, codex = true, opencode = true }

local function adapter_for(state, provider)
	if type(provider) ~= "string" or provider == "" then return nil end
	local cached = state.adapters[provider]
	if cached ~= nil then
		return cached == false and nil or cached
	end
	if not ADAPTER_IDS[provider] then
		state.adapters[provider] = false
		return nil
	end
	-- The extension runtime's require root is the package directory. Adapter
	-- modules therefore use package-relative slash paths, not dotted names.
	local adapter = load_module("adapters/" .. provider .. "/adapter", {
		"adapters/" .. provider .. "/adapter.luau",
		"./adapters/" .. provider .. "/adapter.luau",
		"crates/bootty-app/extensions/bundled-agent-extension/adapters/" .. provider .. "/adapter.luau",
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
	if storage == nil then
		return false, result_error("storage_unavailable", "persistent extension storage is unavailable")
	end
	for _, method in ipairs({ "put", "set", "write", "save" }) do
		local ok, result = safe_call(storage, method, key, copy(value))
		if ok and result ~= false then return true, nil end
	end
	return false, result_error("storage_failed", "persistent extension storage rejected the update", { key = key })
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
	return storage_write(storage_for(state.host), state.storage_key, snapshot)
end

local function publish_changed(state, outcome)
	local payload = {
		topic = "agents.changed",
		revision = state.revision,
		outcome = copy(outcome),
		records = records_array(state),
	}
	local events = is_table(state.host) and state.host.events or nil
	if not is_table(events) then
		return nil, result_error("publication_unavailable", "generic event publication is unavailable", { topic = "agents.changed" })
	end
	local specification = { topic = "agents.changed", payload = payload }
	local ok, result = safe_call(events, "publish", specification)
	if not ok then
		ok, result = safe_call(events, "publish", "agents.changed", payload)
	end
	if not ok then
		ok, result = safe_call(state.host, "publish", "agents.changed", payload)
	end
	if not ok or result == false then
		return nil, result_error("publication_failed", "generic event publication rejected the update", { topic = "agents.changed" })
	end
	return payload, nil
end

local function valid_agent_id(value)
	if type(value) == "string" then return value ~= "" end
	if type(value) == "number" then return value == value and value ~= math.huge and value ~= -math.huge end
	return false
end

local function upsert_record(state, record)
	if not is_table(record) or not valid_agent_id(record.agent_id) then return false end
	local id = record.agent_id
	if state.records[id] == nil then state.order[#state.order + 1] = id end
	state.records[id] = copy(record)
	return true
end

local function commit_records(state, records, removed)
	local old_records = copy(state.records)
	local old_order = copy(state.order)
	local old_revision = state.revision
	local changed = false
	for _, agent_id in ipairs(removed or {}) do
		if valid_agent_id(agent_id) and state.records[agent_id] ~= nil then
			state.records[agent_id] = nil
			for index, current in ipairs(state.order) do
				if current == agent_id then table.remove(state.order, index); break end
			end
			changed = true
		end
	end
	for _, record in ipairs(records or {}) do
		if upsert_record(state, record) then changed = true end
	end
	if not changed then return true, nil, false end
	state.revision = old_revision + 1
	local saved, save_error = save_state(state)
	if not saved then
		state.records, state.order, state.revision = old_records, old_order, old_revision
		return false, save_error, false
	end
	return true, nil, true
end
local function replace_record(state, record)
	return commit_records(state, { record })
end

local function remove_record(state, agent_id)
	if not valid_agent_id(agent_id) or state.records[agent_id] == nil then return true, nil, false end
	local old_records = copy(state.records)
	local old_order = copy(state.order)
	local old_revision = state.revision
	state.records[agent_id] = nil
	for index, id in ipairs(state.order) do
		if id == agent_id then table.remove(state.order, index); break end
	end
	state.revision = old_revision + 1
	local saved, save_error = save_state(state)
	if not saved then
		state.records, state.order, state.revision = old_records, old_order, old_revision
		return false, save_error, false
	end
	return true, nil, true
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
		handles = {},
		pending_starts = {},
		waiters = {},
		wait_subscription = nil,
	}
	local persisted = storage_read(storage_for(state.host), state.storage_key)
	if is_table(persisted) and is_table(persisted.records) then
		for _, record in ipairs(persisted.records) do upsert_record(state, record) end
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
		if record ~= nil
			and (args.provider == nil or record.provider == args.provider)
			and (args.state == nil or (record.lifecycle or record.status) == args.state)
		then
			result[#result + 1] = copy(record)
		end
	end
	return result
end

local function require_agent(state, args)
	local id = args.agent_id
	if not valid_agent_id(id) then
		return nil, result_error("invalid_argument", "agent_id is required", { argument = "agent_id" })
	end
	local record = state.records[id]
	if record == nil then
		return nil, result_error("not_found", "agent record was not found", { agent_id = id })
	end
	return record, nil
end

local function adapter_control(state, command, record, args, method)
	local adapter = adapter_for(state, record.provider)
	if type(adapter) ~= "table" then
		return unsupported(command, record, "adapter control is unavailable")
	end
	local handle = state.handles[record.agent_id]
	if handle == nil and record.process_id ~= nil then
		handle = { process_id = record.process_id, transport = "stdio_jsonl" }
	end
	if not is_table(handle) then
		return unsupported(command, record, "no live managed transport is attached")
	end
	local builder = adapter[method]
	local value, detail
	local control_args = copy(args)
	control_args.native_id = control_args.native_id or record.native_id
	control_args.adapter_session_id = control_args.adapter_session_id or record.adapter_session_id
	control_args.thread_id = control_args.thread_id or record.adapter_session_id or record.native_id
	if type(builder) == "function" then
		local ok
		if method == "prompt" or method == "send_text" then
			ok, value, detail = pcall(builder, control_args.prompt or control_args.text, control_args)
		elseif method == "send_keys" then
			ok, value, detail = pcall(builder, control_args.keys, control_args)
		else
			ok, value, detail = pcall(builder, control_args, record)
		end
		if not ok then
			return result_error("adapter_failed", tostring(value), { provider = record.provider, agent_id = record.agent_id })
		end
		if is_table(value) and value.supported == false then
			return unsupported(command, record, value.diagnostic and value.diagnostic.message or "adapter control is unavailable")
		end
		if is_table(value) and value.ok == false then
			if value.code == "unsupported_control" or value.code == "adapter.unsupported_control" then
				return unsupported(command, record, value.message)
			end
			return result_error(value.code or "adapter_failed", value.message or "adapter rejected control", {
				provider = record.provider,
				agent_id = record.agent_id,
				diagnostic = value.diagnostic,
			})
		end
		if value == nil then
			return unsupported(command, record, "adapter control did not produce a protocol request")
		end
	else
		return unsupported(command, record, "adapter control is unavailable")
	end
	local process = is_table(state.host) and state.host.process or nil
	if handle.process_id ~= nil and is_table(process) and type(process.write) == "function" then
		local encoded_ok, encoded = pcall(function()
			return type(value) == "string" and value or encode_json(value)
		end)
		if not encoded_ok then
			return result_error("adapter_failed", tostring(encoded), { provider = record.provider, agent_id = record.agent_id })
		end
		local ok, response = safe_call(process, "write", {
			process = handle.process_id,
			data = encoded .. "\n",
		})
		if not ok then
			ok, response = safe_call(process, "write", handle.process_id, encoded .. "\n")
		end
		if not ok or response == false then
			return result_error("transport_failed", "managed process rejected the control request", {
				agent_id = record.agent_id,
				process_id = handle.process_id,
			})
		end
		return {
			accepted = true,
			pending = true,
			agent_id = record.agent_id,
			provider = record.provider,
			transport = "process",
			process_id = handle.process_id,
			request = copy(value),
		}
	end
	local ipc = is_table(state.host) and state.host.ipc or nil
	if handle.ipc_id ~= nil and is_table(ipc) and type(ipc.publish) == "function" then
		local ok, response = safe_call(ipc, "publish", {
			topic = handle.ipc_id,
			payload = copy(value),
		})
		if not ok then
			ok, response = safe_call(ipc, "publish", handle.ipc_id, copy(value))
		end
		if ok and response ~= false then
			return {
				accepted = true,
				pending = true,
				agent_id = record.agent_id,
				provider = record.provider,
				transport = "ipc",
				ipc_id = handle.ipc_id,
				request = copy(value),
			}
		end
	end
	return result_error("transport_unavailable", "managed control has no live process or IPC transport", {
		agent_id = record.agent_id,
		provider = record.provider,
	})
end

local function invocation_parts(raw_args)
	if is_table(raw_args) and is_table(raw_args.arguments) and raw_args.command ~= nil then
		return raw_args.arguments, raw_args
	end
	return raw_args, nil
end

local function adapter_mode(provider, event, args)
	if type(args.mode) == "string" and args.mode ~= "" then return args.mode end
	if type(event.mode) == "string" and event.mode ~= "" then return event.mode end
	if provider == "pi" then
		if event.source == "rpc" or event.source == "managed" or event.type == "rpc_event" then return "rpc" end
		return "hook"
	end
	if provider == "claude" then
		if event.hook_event_name ~= nil or event.source == "hook" or event.source == "interactive" then return "hooks" end
		return "managed"
	end
	if provider == "codex" then
		if event.hook_event_name ~= nil or event.source == "hook" or event.source == "interactive" then return "hooks" end
		return "app_server"
	end
	if provider == "opencode" then
		return event.source == "managed" and "managed" or "existing"
	end
	return nil
end

local function normalized_events(adapter, raw_event, mode, context)
	local normalize = adapter.forward or adapter.normalize or adapter.ingest
	if type(normalize) ~= "function" then
		return nil, { diagnostic = { code = "unsupported_normalization", message = "adapter does not expose normalization" } }
	end
	local ok, result = pcall(normalize, raw_event, mode, context)
	if not ok then
		return nil, { diagnostic = { code = "adapter_failed", message = tostring(result) } }
	end
	if not is_table(result) then
		return nil, { diagnostic = { code = "malformed_adapter_result", message = "adapter normalization returned a non-object" } }
	end
	local events = {}
	local diagnostics = {}
	for _, item in ipairs(result.diagnostics or {}) do diagnostics[#diagnostics + 1] = copy(item) end
	if result.diagnostic ~= nil and #diagnostics == 0 then diagnostics[1] = copy(result.diagnostic) end
	if result.event ~= nil and is_table(result.event) then events[#events + 1] = copy(result.event) end
	for _, event in ipairs(result.events or {}) do
		if is_table(event) then events[#events + 1] = copy(event) end
	end
	if result.accepted == false and #diagnostics == 0 then
		diagnostics[1] = { code = "adapter_rejected", message = "adapter rejected the event" }
	end
	return events, { diagnostics = diagnostics, result = result }
end

local function terminal_lifecycle(value)
	return value == "completed" or value == "failed" or value == "aborted" or value == "released"
end

local function record_lifecycle(record)
	if not is_table(record) then return nil end
	local value = record.lifecycle or record.status or record.state
	if value ~= nil then return value end
	local facts = record.facts
	if is_table(facts) then return facts.lifecycle or facts.status or facts.state end
	return nil
end
local function waiter_finish(state, task_id, outcome)
	local tasks = is_table(state.host) and state.host.tasks or nil
	if not is_table(tasks) then return false end
	local ok, result = safe_call(tasks, "finish", {
		task = task_id,
		outcome = copy(outcome),
	})
	if not ok then
		ok, result = safe_call(tasks, "finish", task_id, copy(outcome))
	end
	if not ok or result == false then return false end
	for agent_id, waiters in pairs(state.waiters) do
		waiters[task_id] = nil
		if next(waiters) == nil then state.waiters[agent_id] = nil end
	end
	return true
end

local function waiter_event(state, payload)
	if not is_table(payload) then return end
	local records = payload.records
	if not is_table(records) then
		records = {}
		if is_table(payload.record) then records[1] = payload.record end
	end
	for _, record in ipairs(records) do
		if is_table(record) and valid_agent_id(record.agent_id) then
			local waiters = state.waiters[record.agent_id]
			local lifecycle = record_lifecycle(record)
			if waiters ~= nil and terminal_lifecycle(lifecycle) then
				for task_id in pairs(waiters) do
					waiter_finish(state, task_id, {
						task_id = task_id,
						agent_id = record.agent_id,
						status = lifecycle,
						completed = true,
						timed_out = false,
						record = copy(record),
					})
				end
			end
		end
	end
end

local function ensure_wait_subscription(state)
	if state.wait_subscription ~= nil then return true, nil end
	local events = is_table(state.host) and state.host.events or nil
	if not is_table(events) or type(events.subscribe) ~= "function" then
		return false, result_error("events_unavailable", "agents.wait requires generic event subscription")
	end
	local ok, subscription = safe_call(events, "subscribe", "agents.changed", function(payload)
		waiter_event(state, payload)
	end)
	if not ok then
		return false, result_error("subscription_failed", "agents.wait could not subscribe to agents.changed")
	end
	state.wait_subscription = subscription or { topic = "agents.changed" }
	return true, nil
end

local function poll_wait_events(state)
	local events = is_table(state.host) and state.host.events or nil
	local subscription = state.wait_subscription
	if not is_table(events) or subscription == nil or type(events.poll) ~= "function" then return end
	local id = subscription.subscription or subscription.id or subscription
	local cursor = subscription.cursor or 0
	local ok, delivery = safe_call(events, "poll", { subscription = id, cursor = cursor })
	if not ok then
		ok, delivery = safe_call(events, "poll", id, cursor)
	end
	if not ok or not is_table(delivery) then return end
	subscription.cursor = delivery.cursor or cursor
	for _, payload in ipairs(delivery.events or delivery.items or {}) do waiter_event(state, payload) end
end

local function wait_for_agent(state, record, args)
	local tasks = is_table(state.host) and state.host.tasks or nil
	if not is_table(tasks) or type(tasks.start) ~= "function" then
		return result_error("tasks_unavailable", "agents.wait requires generic task support")
	end
	local subscribed, subscription_error = ensure_wait_subscription(state)
	if not subscribed then return subscription_error end
	local timeout_ms = tonumber(args.timeout_ms) or 0
	if timeout_ms < 0 then
		return result_error("invalid_argument", "timeout_ms must not be negative", { argument = "timeout_ms" })
	end
	local ok, task = safe_call(tasks, "start", {
		kind = "agents.wait",
		agent_id = record.agent_id,
		timeout_ms = timeout_ms,
	})
	if not ok or not is_table(task) or task.id == nil then
		return result_error("task_start_failed", "agents.wait task could not be created", { agent_id = record.agent_id })
	end
	local task_id = task.id
	state.waiters[record.agent_id] = state.waiters[record.agent_id] or {}
	state.waiters[record.agent_id][task_id] = true
	poll_wait_events(state)
	local lifecycle = record_lifecycle(record)
	if terminal_lifecycle(lifecycle) then
		if not waiter_finish(state, task_id, {
			task_id = task_id,
			agent_id = record.agent_id,
			status = lifecycle,
			completed = true,
			timed_out = false,
			record = copy(record),
		}) then
			return result_error("task_finish_failed", "agents.wait could not finish its task", { task_id = task_id })
		end
		return { task_id = task_id, agent_id = record.agent_id, status = lifecycle, completed = true, timed_out = false, record = copy(record) }
	end
	if timeout_ms == 0 then
		if not waiter_finish(state, task_id, {
			task_id = task_id,
			agent_id = record.agent_id,
			status = "timeout",
			completed = false,
			timed_out = true,
		}) then
			return result_error("task_finish_failed", "agents.wait could not finish its timeout task", { task_id = task_id })
		end
		return { task_id = task_id, agent_id = record.agent_id, status = "timeout", completed = false, timed_out = true }
	end
	return { task_id = task_id, agent_id = record.agent_id, status = "waiting", completed = false, timed_out = false, timeout_ms = timeout_ms }
end
local function start_managed(state, args, context)
	local provider = args.provider
	local adapter = adapter_for(state, provider)
	if type(adapter) ~= "table" then
		return result_error("unsupported_adapter", "requested adapter is unavailable", { provider = provider })
	end
	local launch
	if type(adapter.start) == "function" then
		local ok, value = pcall(adapter.start, args, context)
		if not ok then return result_error("adapter_failed", tostring(value), { provider = provider }) end
		launch = value
	end
	launch = is_table(launch) and launch or ADAPTER_MANIFESTS[provider]
	if not is_table(launch) or launch.supported == false then
		return unsupported("agents.start", nil, "adapter does not expose a managed launch protocol")
	end
	local process = is_table(state.host) and state.host.process or nil
	if not is_table(process) or type(process.spawn) ~= "function" then
		return result_error("process_unavailable", "managed start requires the generic supervised process API", { provider = provider })
	end
	local executable = launch.executable or launch.command
	local arguments = launch.arguments or launch.args
	if type(executable) ~= "string" or executable == "" then
		return result_error("invalid_launch", "adapter launch metadata has no executable", { provider = provider })
	end
	if not is_table(arguments) then arguments = {} end
	local spec = {
		executable = executable,
		arguments = copy(args.argv or arguments),
		cwd = args.cwd,
		environment = copy(args.environment),
		protocol = launch.transport or launch.protocol,
		framing = launch.framing,
	}
	local ok, status = safe_call(process, "spawn", spec)
	if not ok or status == false or status == nil then
		return result_error("process_spawn_failed", "managed adapter process could not be started", { provider = provider })
	end
	local process_id = is_table(status) and (status.id or status.process_id) or status
	if process_id == nil then
		return result_error("process_spawn_failed", "managed process did not return an opaque handle", { provider = provider })
	end
	local handle = {
		process_id = process_id,
		provider = provider,
		transport = launch.transport or launch.protocol,
	}
	if is_table(status) and status.ipc_id ~= nil then handle.ipc_id = status.ipc_id end
	state.pending_starts[provider] = handle
	return {
		accepted = true,
		managed = true,
		pending = true,
		provider = provider,
		process_id = process_id,
		transport = handle.transport,
	}
end
local function canonical_fallback(raw_event, provider)
	if not is_table(raw_event) then return nil end
	if raw_event.kind == nil and raw_event.native_id == nil and raw_event.record == nil then return nil end
	local event = copy(raw_event)
	event.adapter = event.adapter or provider
	event.source = event.source or provider
	if event.raw == nil then event.raw = copy(raw_event) end
	return event
end
local function handle_command(state, command_id, raw_args)
	local id = ALIASES[command_id] or command_id
	local raw_arguments, context = invocation_parts(raw_args)
	local args = normalize_args(id, raw_arguments)
	if not is_table(args) then args = {} end
	state.last_context = context
	if id == "agents.list" then return command_list(state, args) end
	if id == "agents.manifest.list" then return manifest_list(args.provider) end
	if id == "agents.manifest.reload" then
		return { reloaded = true, manifest_id = args.manifest_id, manifests = manifest_list(args.manifest_id) }
	end
	if id == "agents.integration.list" then return manifest_list(args.provider) end
	if id == "agents.integration.install"
		or id == "agents.integration.setup"
		or id == "agents.integration.uninstall"
		or id == "agents.integration.doctor"
	then
		local provider = args.provider
		if type(provider) ~= "string" or provider == "" then
			return result_error("invalid_argument", "provider is required", { argument = "provider" })
		end
		local installer = state.installers or load_module("install", {
			"install.luau",
			"./install.luau",
			"crates/bootty-app/extensions/bundled-agent-extension/install.luau",
		})
		if is_table(installer) then
			local method = ({
				["agents.integration.install"] = "integration_install",
				["agents.integration.setup"] = "integration_setup",
				["agents.integration.uninstall"] = "integration_uninstall",
				["agents.integration.doctor"] = "integration_doctor",
			})[id]
			if type(installer[method]) == "function" then
				local ok, value = pcall(installer[method], provider, args, context)
				if ok then return value end
				return result_error("installer_failed", tostring(value), { provider = provider })
			end
		end
		return result_error("unsupported", "installer workflow is unavailable", { provider = provider })
	end
	if id == "agents.ingest" then
		local raw_event = copy(args.event)
		if not is_table(raw_event) then
			return result_error("invalid_argument", "event must be an object", { argument = "event" })
		end
		if raw_event.event ~= nil and type(raw_event.event) ~= "table" and type(raw_event.event) ~= "string" then
			return result_error("invalid_argument", "event kind must be an object or JSON string", { argument = "event.event" })
		end
		local requested_adapter = args.adapter
		if requested_adapter ~= nil and (type(requested_adapter) ~= "string" or requested_adapter == "") then
			return result_error("invalid_argument", "adapter must be a non-empty string", { argument = "adapter" })
		end
		local existing_adapter = raw_event.adapter or raw_event.provider
		if existing_adapter == nil and ADAPTER_IDS[raw_event.source] then existing_adapter = raw_event.source end
		if existing_adapter ~= nil and (type(existing_adapter) ~= "string" or existing_adapter == "") then
			return result_error("invalid_argument", "event adapter must be a non-empty string", { argument = "event.adapter" })
		end
		if existing_adapter ~= nil and not ADAPTER_IDS[existing_adapter] then
			return result_error("unsupported_adapter", "event adapter is not bundled", { adapter = existing_adapter })
		end
		if requested_adapter ~= nil and existing_adapter ~= nil and existing_adapter ~= requested_adapter then
			return result_error("conflicting_argument", "adapter conflicts with the authoritative event source", {
				adapter = requested_adapter,
				source = existing_adapter,
			})
		end
		local provider = requested_adapter or existing_adapter
		if provider == nil then
			return result_error("unsupported_adapter", "agents.ingest requires a bundled adapter", {
				adapter = requested_adapter,
			})
		end
		local adapter = adapter_for(state, provider)
		if type(adapter) ~= "table" then
			return result_error("unsupported_adapter", "requested adapter is unavailable", { adapter = provider })
		end
		local event_name = args.event_name
		if event_name ~= nil and (type(event_name) ~= "string" or event_name == "") then
			return result_error("invalid_argument", "event_name must be a non-empty string", { argument = "event_name" })
		end
		if event_name ~= nil then
			local existing_name = raw_event.hook_event_name or raw_event.event_name
			if existing_name ~= nil and existing_name ~= event_name then
				return result_error("conflicting_argument", "event_name conflicts with the authoritative event kind", {
					event_name = event_name,
					event_type = existing_name,
				})
			end
			if provider == "claude" and raw_event.hook_event_name == nil then raw_event.hook_event_name = event_name end
			raw_event.event_name = raw_event.event_name or event_name
		end
		raw_event.adapter = raw_event.adapter or provider
		raw_event.source = raw_event.source or provider
		if raw_event.raw == nil then raw_event.raw = copy(args.event) end
		local mode = adapter_mode(provider, raw_event, args)
		local normalization_context = is_table(context) and context or args.context
		local normalized, adapter_result = normalized_events(adapter, raw_event, mode, normalization_context)
		local diagnostics = adapter_result and adapter_result.diagnostics or {}
		if #diagnostics == 0 and normalized == nil then
			return result_error("adapter_diagnostic", "adapter normalization failed", {
				adapter = provider,
				diagnostics = diagnostics,
			})
		end
		if normalized == nil or #normalized == 0 then
			local fallback = canonical_fallback(raw_event, provider)
			if fallback ~= nil then
				normalized = { fallback }
			else
				return result_error("adapter_diagnostic", "adapter did not produce an authoritative event", {
					adapter = provider,
					diagnostics = diagnostics,
				})
			end
		end
		local model = state.model
		if model == nil then
			local ok, loaded = pcall(function()
				return load_module("model", { "model.luau", "./model.luau", "crates/bootty-app/extensions/bundled-agent-extension/model.luau" })
			end)
			if ok then model = loaded end
			state.model = model
		end
		if type(model) ~= "table" or type(model.apply_event) ~= "function" then
			return result_error("runtime_unavailable", "normalized model is unavailable")
		end
		state.model_store = state.model_store or model.new_store({ max_events = 512 })
		local records = {}
		local removed = {}
		local seen = {}
		local outcomes = {}
		local changed = false
		for _, normalized_event in ipairs(normalized) do
			normalized_event.adapter = normalized_event.adapter or provider
			normalized_event.source = normalized_event.source or provider
			if event_name ~= nil then normalized_event.event_name = event_name end
			if is_table(raw_event.record) and normalized_event.record == nil then normalized_event.record = copy(raw_event.record) end
			local outcome = model.apply_event(state.model_store, normalized_event)
			outcomes[#outcomes + 1] = outcome
			if outcome.changed then changed = true end
			for _, agent_id in ipairs(outcome.removed or {}) do
				if valid_agent_id(agent_id) then removed[#removed + 1] = agent_id end
			end
			local candidates = outcome.records or {}
			if is_table(outcome.record) and valid_agent_id(outcome.record.agent_id) then
				candidates[#candidates + 1] = outcome.record
			end
			for _, record in ipairs(candidates) do
				if is_table(record) and valid_agent_id(record.agent_id) and not seen[record.agent_id] then
					seen[record.agent_id] = true
					records[#records + 1] = record
					local pending = state.pending_starts[provider]
					if pending ~= nil then
						state.handles[record.agent_id] = pending
						state.pending_starts[provider] = nil
					end
				end
			end
		end
		if changed then
			local committed, commit_error, did_change = commit_records(state, records, removed)
			if not committed then return commit_error end
			if did_change then
				local published, publication_error = publish_changed(state, {
					kind = "ingest",
					adapter = provider,
					outcomes = outcomes,
					diagnostics = diagnostics,
				})
				if published == nil then return publication_error end
				waiter_event(state, published)
				return {
					accepted = true,
					changed = true,
					status = outcomes[#outcomes] and outcomes[#outcomes].status or "accepted",
					event = outcomes[#outcomes] and outcomes[#outcomes].event,
					records = records_array(state),
					revision = state.revision,
					published = published,
					diagnostics = diagnostics,
				}
			end
		end
		return {
			accepted = true,
			changed = false,
			status = outcomes[#outcomes] and outcomes[#outcomes].status or "accepted",
			event = outcomes[#outcomes] and outcomes[#outcomes].event,
			records = records_array(state),
			revision = state.revision,
			diagnostics = diagnostics,
		}
	end
	if id == "agents.get" or id == "agents.read" or id == "agents.explain" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		if id == "agents.explain" then
			return {
				agent_id = record.agent_id,
				provider = record.provider,
				lifecycle = record.lifecycle,
				authority = copy(record.authority),
				facts = copy(record.facts or record),
			}
		end
		if id == "agents.read" then return adapter_control(state, id, record, args, "read") end
		return { agent_id = record.agent_id, record = copy(record) }
	end
	if id == "agents.report" then
		local agent_id = args.agent_id
		if not valid_agent_id(agent_id) then return result_error("invalid_argument", "agent_id is required", { argument = "agent_id" }) end
		local record = state.records[agent_id] or { agent_id = agent_id }
		if args.provider ~= nil then record.provider = args.provider end
		if args.facts ~= nil then record.facts = copy(args.facts) end
		local committed, commit_error = commit_records(state, { record })
		if not committed then return commit_error end
		local published, publication_error = publish_changed(state, { kind = "report", record = record })
		if published == nil then return publication_error end
		waiter_event(state, published)
		return { agent_id = agent_id, record = copy(record), reported = true, published = published, revision = state.revision }
	end
	if id == "agents.report_session" then
		local provider, native_id = args.provider, args.native_session_id
		if type(provider) ~= "string" or provider == "" or not valid_agent_id(native_id) then
			return result_error("invalid_argument", "provider and native_session_id are required")
		end
		for _, agent_id in ipairs(state.order) do
			local record = state.records[agent_id]
			if record ~= nil and record.provider == provider
				and (record.native_id == native_id or record.adapter_session_id == native_id)
			then
				return { agent_id = record.agent_id, record = copy(record), reported = true }
			end
		end
		if type(adapter_for(state, provider)) ~= "table" then
			return result_error("unsupported_adapter", "requested adapter is unavailable", { adapter = provider })
		end
		return {
			accepted = true,
			pending = true,
			provider = provider,
			native_session_id = native_id,
			diagnostic = {
				code = "awaiting_authoritative_session",
				message = "the adapter has not published an agent identity for this opaque session",
			},
		}
	end
	if id == "agents.start" then
		if type(args.provider) ~= "string" or args.provider == "" then
			return result_error("invalid_argument", "provider is required", { argument = "provider" })
		end
		if args.managed == false then
			return result_error("unsupported", "interactive sessions are started by the native integration")
		end
		return start_managed(state, args, context)
	end
	if id == "agents.focus" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local sessions = is_table(state.host) and state.host.sessions or nil
		if is_table(sessions) then
			local ok, value = safe_call(sessions, "focus", record.terminal_id or (record.terminal and record.terminal.terminal_id), copy(record))
			if ok and value ~= false then return value end
		end
		return unsupported(id, record, "terminal focus is unavailable")
	end
	if id == "agents.rename" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		if type(args.name) ~= "string" or args.name == "" then return result_error("invalid_argument", "name is required", { argument = "name" }) end
		local updated = copy(record)
		updated.name = args.name
		local committed, commit_error = commit_records(state, { updated })
		if not committed then return commit_error end
		local published, publication_error = publish_changed(state, { kind = "rename", record = updated })
		if published == nil then return publication_error end
		return { changed = true, agent_id = updated.agent_id, record = updated, published = published, revision = state.revision }
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
		return wait_for_agent(state, record, args)
	end
	if id == "agents.pause" or id == "agents.resume" or id == "agents.retry" or id == "agents.abort" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		return adapter_control(state, id, record, args, id:sub(8))
	end
	if id == "agents.view.set" or id == "agents.view.clear" or id == "agents.clear_authority" then
		if id == "agents.view.clear" and args.agent_id == nil then
			local updates = {}
			for _, agent_id in ipairs(state.order) do
				local record = state.records[agent_id]
				if record ~= nil and record.view ~= nil then
					local updated = copy(record)
					updated.view = nil
					updates[#updates + 1] = updated
				end
			end
			local committed, commit_error, changed = commit_records(state, updates)
			if not committed then return commit_error end
			local published
			if changed then
				local publication, publication_error = publish_changed(state, { kind = id })
				if publication == nil then return publication_error end
				published = publication
			end
			return { changed = changed, cleared = true, published = published }
		end
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		local updated = copy(record)
		if id == "agents.view.set" then updated.view = copy(args.surface)
		elseif id == "agents.view.clear" then updated.view = nil
		else updated.authority = nil end
		local committed, commit_error = commit_records(state, { updated })
		if not committed then return commit_error end
		local published, publication_error = publish_changed(state, { kind = id, record = updated })
		if published == nil then return publication_error end
		return { changed = true, agent_id = updated.agent_id, record = updated, published = published, revision = state.revision }
	end
	if id == "agents.release" then
		local record, error_result = require_agent(state, args)
		if error_result ~= nil then return error_result end
		-- Release is ownership cleanup; lifecycle remains adapter-authoritative.
		state.handles[record.agent_id] = nil
		return { released = true, agent_id = record.agent_id, record = copy(record) }
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
