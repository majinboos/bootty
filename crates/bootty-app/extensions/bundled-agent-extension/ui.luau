--!strict
-- Generic UI/model helpers for the bundled agents extension.
--
-- Runner contract (kept in code so fixture runners do not need project integration):
--   1. Create a fresh Lua/Luau state with `bootty` replaced by a deterministic fake when needed.
--   2. Load this file with `dofile`/`loadfile`; it returns a table of pure helpers.
--   3. Load each `tests/*.luau` file with the returned module and a fixture table.  A test may
--      return a function, a table of functions, or `{ run = function(...) ... end }`.
--   4. The runner calls every test with `(ui, fixtures, fake_host)` in lexical/file order and
--      treats `false`, `nil, false`, or a raised error as failure.  Tests use only deterministic
--      fixture data and must not spawn processes, inspect the terminal, read the clock, or make
--      network requests.
--   5. The same files run under Lua 5.4 and Luau; keep fixture/test syntax in their common
--      language subset.  A host may provide `bootty.commands.invoke` and `bootty.events.subscribe`.
--
-- This module intentionally does not own widgets, terminal parsing, process inspection, or
-- lifecycle inference.  The extension model supplies normalized records and explicit events; this
-- module only copies, orders, and presents those facts.  IDs are opaque strings and are never
-- split, normalized, or reconstructed from terminal/process fields.

local ui = {}

local LIFECYCLES = {
	unknown = true,
	starting = true,
	idle = true,
	running = true,
	waiting = true,
	completed = true,
	failed = true,
	aborted = true,
	paused = true,
	stale = true,
}
ui.lifecycle_values = {
	"unknown",
	"starting",
	"idle",
	"running",
	"waiting",
	"completed",
	"failed",
	"aborted",
	"paused",
	"stale",
}

-- The bundled extension's catalog contract. Keep this list explicit and ordered: tests and hosts
-- can compare it with the generated 29-entry manifest without depending on map iteration order.
ui.command_ids = {
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


-- Commands that can be rendered as row actions. Catalog/handler commands that have no agent row
-- target remain available through `invoke` and the workflow helpers below.
ui.row_action_ids = {
	"agents.prompt",
	"agents.read",
	"agents.explain",
	"agents.focus",
	"agents.send_text",
	"agents.send_keys",
	"agents.wait",
	"agents.pause",
	"agents.resume",
	"agents.retry",
	"agents.abort",
	"agents.rename",
	"agents.view.set",
	"agents.view.clear",
	"agents.clear_authority",
	"agents.release",
}

local FACT_FIELDS = {
	"prompt",
	"turn",
	"tool",
	"permission",
	"usage",
	"model",
	"reasoning",
	"compaction",
	"notification",
	"error",
	"fidelity",
	"updated_at",
}

local RECORD_OBJECT_FIELDS = {
	"native_ids",
	"authority",
	"terminal",
	"facts",
	"view",
	"usage",
	"reasoning",
	"compaction",
	"notification",
	"error",
	"fidelity",
	"evidence",
}

local function is_table(value)
	return type(value) == "table"
end

local function copy(value, seen)
	if not is_table(value) then
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

ui.copy = copy

local function string_or_nil(value)
	if value == nil then
		return nil
	end
	if type(value) == "string" then
		return value
	end
	return tostring(value)
end

local function number_or_nil(value)
	if type(value) == "number" then
		return value
	end
	if type(value) == "string" and value ~= "" then
		return tonumber(value)
	end
	return nil
end

local function first_present(table_value, keys)
	if not is_table(table_value) then
		return nil, nil
	end
	for _, key in ipairs(keys) do
		if table_value[key] ~= nil then
			return table_value[key], key
		end
	end
	return nil, nil
end

local function record_id(record)
	if not is_table(record) then
		return nil
	end
	-- `id` is deliberately not accepted here. Native/session/terminal identifiers are opaque and
	-- must not become bundle agent identity by accident.
	if record.agent_id == nil then
		return nil
	end
	return string_or_nil(record.agent_id)
end

ui.record_id = record_id

local function lifecycle_value(record)
	if not is_table(record) then
		return "unknown"
	end
	local value = record.lifecycle
	if value == nil then
		-- `status`/`state` are explicit compatibility spellings from early model snapshots, not
		-- inferred process state. Unknown values remain visible instead of being guessed.
		value = record.status or record.state
	end
	if value == nil and is_table(record.facts) then
		-- Some model snapshots retain partial facts under `facts`; consuming an explicit lifecycle
		-- fact is not the same as deriving one from terminal/process observations.
		value = record.facts.lifecycle or record.facts.status or record.facts.state
	end
	if value == nil then
		return "unknown"
	end
	return string_or_nil(value) or "unknown"
end

ui.lifecycle_value = lifecycle_value

local function copy_known_field(source, target, key)
	if source[key] ~= nil then
		target[key] = copy(source[key])
	end
end

-- Normalize one model record. Missing facts remain missing; only lifecycle receives the explicit
-- `unknown` sentinel because the normalized contract needs a total lifecycle value. No process,
-- terminal, native-session, or transcript field is consulted to fill another field.
function ui.normalize(record)
	if not is_table(record) then
		return nil
	end
	local id = record_id(record)
	if id == nil or id == "" then
		return nil
	end
	local normalized = { agent_id = id, lifecycle = lifecycle_value(record) }
	local scalar_fields = {
		"name",
		"provider",
		"prompt",
		"turn",
		"tool",
		"permission",
		"model",
		"updated_at",
		"generation",
		"sequence",
		"managed",
		"stale",
	}
	for _, key in ipairs(scalar_fields) do
		copy_known_field(record, normalized, key)
	end
	for _, key in ipairs(RECORD_OBJECT_FIELDS) do
		copy_known_field(record, normalized, key)
	end
	if record.status ~= nil and record.lifecycle == nil then
		-- Preserve the compatibility source only when it was actually supplied.
		normalized.status = copy(record.status)
	end
	return normalized
end

ui.normalize_record = ui.normalize

local function array_values(values)
	local result = {}
	if not is_table(values) then
		return result
	end
	local numeric = true
	local count = 0
	for key in pairs(values) do
		if type(key) ~= "number" then
			numeric = false
			break
		end
		if key > count then
			count = key
		end
	end
	if numeric then
		for index = 1, count do
			result[#result + 1] = values[index]
		end
		return result
	end
	local keys = {}
	for key in pairs(values) do
		keys[#keys + 1] = key
	end
	table.sort(keys, function(left, right)
		return tostring(left) < tostring(right)
	end)
	for _, key in ipairs(keys) do
		result[#result + 1] = values[key]
	end
	return result
end

local function explicit_sequence(value)
	if not is_table(value) then
		return nil
	end
	local direct = first_present(value, { "sequence", "seq", "revision", "cursor" })
	if direct ~= nil then
		return number_or_nil(direct)
	end
	if is_table(value.authority) then
		return number_or_nil(value.authority.sequence)
	end
	if is_table(value.record) then
		return explicit_sequence(value.record)
	end
	return nil
end

local function explicit_generation(value)
	if not is_table(value) then
		return nil
	end
	local direct = first_present(value, { "generation", "connection_generation", "epoch" })
	if direct ~= nil then
		return number_or_nil(direct)
	end
	if is_table(value.record) then
		return explicit_generation(value.record)
	end
	return nil
end

ui.explicit_generation = explicit_generation


local function values_with_explicit_map_ids(values)
	if not is_table(values) then
		return {}
	end
	local numeric = true
	for key in pairs(values) do
		if type(key) ~= "number" then
			numeric = false
			break
		end
	end
	if numeric then
		return array_values(values)
	end
	local keys = {}
	for key in pairs(values) do
		keys[#keys + 1] = key
	end
	table.sort(keys, function(left, right)
		return tostring(left) < tostring(right)
	end)
	local result = {}
	for _, key in ipairs(keys) do
		local value = values[key]
		if is_table(value) and record_id(value) == nil and type(key) == "string" and key ~= "" then
			value = copy(value)
			value.agent_id = key
		end
		result[#result + 1] = value
	end
	return result
end

local function event_operation(event)
	if not is_table(event) then
		return "upsert"
	end
	local operation = first_present(event, { "operation", "op", "kind", "type" })
	if operation == nil and event.rebase == true then
		operation = "rebase"
	end
	if operation == nil then
		if event.snapshot ~= nil or event.records ~= nil then
			return "snapshot"
		end
		return "upsert"
	end
	return string.lower(string_or_nil(operation) or "upsert")
end

ui.event_operation = event_operation

local function is_rebase_operation(operation)
	return operation == "rebase"
		or operation == "snapshot"
		or operation == "replace"
		or operation == "reset"
		or operation == "rebased"
end

local function event_record_from(value, envelope)
	if not is_table(value) then
		return nil
	end
	if record_id(value) ~= nil then
		return value
	end
	local id = record_id(envelope)
	if id == nil then
		return nil
	end
	local result = copy(value)
	result.agent_id = id
	if is_table(envelope) and is_table(envelope.facts) then
		for key, fact in pairs(envelope.facts) do
			if result[key] == nil then
				result[key] = copy(fact)
			end
		end
	end
	if is_table(envelope) and (envelope.terminal_id ~= nil or envelope.process_id ~= nil) then
		result.terminal = copy(result.terminal or {})
		if result.terminal.terminal_id == nil then
			result.terminal.terminal_id = copy(envelope.terminal_id)
		end
		if result.terminal.process_id == nil then
			result.terminal.process_id = copy(envelope.process_id)
		end
	end
	return result
end

local function unwrap_event_record(event)
	if not is_table(event) then
		return nil
	end
	local direct = event.record or event.update or event.agent or event.value
	local resolved = event_record_from(direct, event)
	if resolved ~= nil then
		return resolved
	end
	if record_id(event) ~= nil then
		return event
	end
	if is_table(event.data) then
		if record_id(event.data) ~= nil then
			return event.data
		end
		resolved = event_record_from(event.data.record or event.data.update or event.data.agent, event)
		if resolved ~= nil then
			return resolved
		end
	end
	return nil
end

ui.unwrap_event_record = unwrap_event_record

local function event_records(event)
	if not is_table(event) then
		return {}
	end
	local values = event.records
	if values == nil and is_table(event.snapshot) then
		if record_id(event.snapshot) ~= nil then
			return { event.snapshot }
		end
		values = event.snapshot.records or event.snapshot.agents or event.snapshot
	end
	if values == nil and is_table(event.data) then
		values = event.data.records or event.data.agents
	end
	if values == nil then
		local record = unwrap_event_record(event)
		return record and { record } or {}
	end
	return values_with_explicit_map_ids(values)
end

ui.event_records = event_records


local function stable_value(value, seen)
	if not is_table(value) then
		return tostring(value)
	end
	seen = seen or {}
	if seen[value] then
		return "<cycle>"
	end
	seen[value] = true
	local keys = {}
	for key in pairs(value) do
		keys[#keys + 1] = key
	end
	table.sort(keys, function(left, right)
		return tostring(left) < tostring(right)
	end)
	local parts = {}
	for _, key in ipairs(keys) do
		parts[#parts + 1] = tostring(key) .. "=" .. stable_value(value[key], seen)
	end
	seen[value] = nil
	return "{" .. table.concat(parts, ",") .. "}"
end

local function stable_event_key(event, index)
	if not is_table(event) then
		return "event:" .. tostring(index)
	end
	local explicit = event.event_id or event.eventId
	if explicit ~= nil then
		local source = event.source or event.provider or event.adapter
		return "id:" .. stable_value(source) .. ":" .. tostring(explicit)
	end
	local operation = event_operation(event)
	local record = unwrap_event_record(event)
	local id = record_id(record) or string_or_nil(event.agent_id)
	local sequence = explicit_sequence(event)
	local generation = explicit_generation(event)
	if id ~= nil and (sequence ~= nil or generation ~= nil) then
		return "agent:"
			.. id
			.. ":generation:"
			.. tostring(generation)
			.. ":sequence:"
			.. tostring(sequence)
			.. ":"
			.. operation
	end
	if id ~= nil then
		return "agent:" .. id .. ":payload:" .. stable_value(record or event) .. ":" .. operation
	end
	return "event:" .. tostring(index) .. ":" .. stable_value(event)
end
ui.event_key = stable_event_key

-- Stable ordering: explicit sequence/revision/cursor is authoritative; missing sequence keeps the
-- source order and follows sequenced records. We never sort by lifecycle text, provider, PID, or
-- any other guessed activity signal.
function ui.order_events(events)
	local values = array_values(events)
	local decorated = {}
	for index, event in ipairs(values) do
		decorated[#decorated + 1] = {
			event = event,
			index = index,
			generation = explicit_generation(event),
			sequence = explicit_sequence(event),
		}
	end
	table.sort(decorated, function(left, right)
		if left.generation == nil and right.generation ~= nil then
			return false
		end
		if left.generation ~= nil and right.generation == nil then
			return true
		end
		if left.generation ~= nil and right.generation ~= nil and left.generation ~= right.generation then
			return left.generation < right.generation
		end
		if left.sequence == nil and right.sequence ~= nil then
			return false
		end
		if left.sequence ~= nil and right.sequence == nil then
			return true
		end
		if left.sequence ~= nil and right.sequence ~= nil and left.sequence ~= right.sequence then
			return left.sequence < right.sequence
		end
		return left.index < right.index
	end)
	local ordered = {}
	for _, item in ipairs(decorated) do
		ordered[#ordered + 1] = item.event
	end
	return ordered
end

ui.lifecycle_order = ui.order_events

function ui.dedupe_events(events)
	local result = {}
	local seen = {}
	for index, event in ipairs(ui.order_events(events)) do
		local key = stable_event_key(event, index)
		if not seen[key] then
			seen[key] = true
			result[#result + 1] = event
		end
	end
	return result
end

local function record_sequence(record)
	return explicit_sequence(record)
end

local function should_replace(existing, incoming)
	if existing == nil then
		return true
	end
	local old_generation = explicit_generation(existing)
	local new_generation = explicit_generation(incoming)
	if old_generation ~= nil and new_generation ~= nil and old_generation ~= new_generation then
		return new_generation > old_generation
	end
	if old_generation ~= nil and new_generation == nil then
		return false
	end
	local old_sequence = record_sequence(existing)
	local new_sequence = record_sequence(incoming)
	if old_sequence ~= nil and new_sequence ~= nil then
		return new_sequence >= old_sequence
	end
	-- An explicitly sequenced record is authoritative over an unsequenced partial fact. For two
	-- unsequenced events, source/event order is the only explicit ordering available.
	if old_sequence ~= nil and new_sequence == nil then
		return false
	end
	return true
end

local function add_record(state, record, sequence, generation)
	local normalized = ui.normalize(record)
	if normalized == nil then
		return
	end
	if sequence ~= nil and explicit_sequence(normalized) == nil then
		normalized.authority = copy(normalized.authority or {})
		normalized.authority.sequence = sequence
	end
	if generation ~= nil and explicit_generation(normalized) == nil then
		normalized.generation = generation
	end
	local id = normalized.agent_id
	local previous = state.by_id[id]
	if not should_replace(previous, normalized) then
		return
	end
	if previous == nil then
		state.order[#state.order + 1] = id
	end
	state.by_id[id] = normalized
end

local function remove_record(state, id)
	if id == nil then
		return
	end
	id = string_or_nil(id)
	if id == nil or state.by_id[id] == nil then
		return
	end
	state.by_id[id] = nil
	for index, value in ipairs(state.order) do
		if value == id then
			table.remove(state.order, index)
			break
		end
	end
end

local function state_records(state)
	local result = {}
	for _, id in ipairs(state.order) do
		local record = state.by_id[id]
		if record ~= nil then
			result[#result + 1] = copy(record)
		end
	end
	return result
end

local function observe_seed_cursor(state, record)
	local generation = explicit_generation(record)
	local sequence = explicit_sequence(record)
	if generation ~= nil and (state.generation == nil or generation > state.generation) then
		state.generation = generation
		state.cursor = sequence
		state.revision = sequence
	elseif generation == state.generation and sequence ~= nil and (state.cursor == nil or sequence > state.cursor) then
		state.cursor = sequence
		state.revision = sequence
	elseif state.generation == nil and sequence ~= nil and (state.cursor == nil or sequence > state.cursor) then
		state.cursor = sequence
		state.revision = sequence
	end
end

function ui.new_state(seed)
	local state = {
		by_id = {},
		order = {},
		seen = {},
		sources = {},
		generation = nil,
		cursor = nil,
		revision = nil,
	}
	for _, record in ipairs(values_with_explicit_map_ids(seed)) do
		add_record(state, record, explicit_sequence(record), explicit_generation(record))
		observe_seed_cursor(state, record)
	end
	state.records = state_records(state)
	return state
end

ui.state_records = state_records

local function snapshot_values(event)
	local values = event_records(event)
	local result = {}
	for _, record in ipairs(values) do
		if record_id(record) ~= nil then
			result[#result + 1] = record
		end
	end
	return result
end

local function event_agent_id(event)
	local record = unwrap_event_record(event)
	if record ~= nil then
		return record_id(record)
	end
	if is_table(event) then
		return string_or_nil(event.agent_id)
	end
	return nil
end

local function event_source(event)
	if not is_table(event) then
		return ""
	end
	local source = event.source or event.provider or event.adapter
	return string_or_nil(source) or ""
end

local function update_state_cursors(state, event)
	local generation = explicit_generation(event)
	local sequence = explicit_sequence(event)
	local source = event_source(event)
	state.sources = state.sources or {}
	local cursor = state.sources[source] or { generation = nil, cursor = nil }
	if generation ~= nil then
		cursor.generation = generation
		state.generation = generation
	end
	if sequence ~= nil then
		cursor.cursor = sequence
		state.cursor = sequence
		state.revision = sequence
	end
	state.sources[source] = cursor
end

function ui.apply_event(state, event)
	if not is_table(state) or not is_table(event) then
		return state
	end
	local key = stable_event_key(event, #state.seen + 1)
	if state.seen[key] then
		return state
	end
	state.seen[key] = true
	local generation = explicit_generation(event)
	local sequence = explicit_sequence(event)
	local source = event_source(event)
	state.sources = state.sources or {}
	local cursor = state.sources[source] or { generation = nil, cursor = nil }
	if generation ~= nil and cursor.generation ~= nil and generation < cursor.generation then
		state.records = state_records(state)
		return state
	end
	if generation ~= nil and (cursor.generation == nil or generation > cursor.generation) then
		cursor.generation = generation
		cursor.cursor = nil
	end
	if generation == cursor.generation and sequence ~= nil and cursor.cursor ~= nil and sequence <= cursor.cursor then
		state.records = state_records(state)
		return state
	end
	local operation = event_operation(event)
	if is_rebase_operation(operation) then
		state.by_id = {}
		state.order = {}
		for _, record in ipairs(snapshot_values(event)) do
			add_record(state, record, explicit_sequence(record) or sequence, explicit_generation(record) or generation)
		end
		update_state_cursors(state, event)
		state.records = state_records(state)
		return state
	end
	if operation == "remove" or operation == "delete" or operation == "removed" then
		remove_record(state, event_agent_id(event))
		update_state_cursors(state, event)
		state.records = state_records(state)
		return state
	end
	for _, record in ipairs(event_records(event)) do
		add_record(state, record, sequence, generation)
	end
	update_state_cursors(state, event)
	state.records = state_records(state)
	return state
end

local function event_list(events)
	if is_table(events) and is_table(events.events) then
		return events.events
	end
	return events
end

function ui.reduce_state(events, seed)
	local state = ui.new_state(seed)
	for _, event in ipairs(ui.order_events(event_list(events))) do
		ui.apply_event(state, event)
	end
	state.records = state_records(state)
	return state
end

-- First return is the render-friendly normalized record array; the optional second return keeps
-- cursor/revision state available to callers that need to continue consuming agents.changed.
function ui.reduce(events, seed)
	local state = ui.reduce_state(events, seed)
	return state.records, state
end

function ui.normalize_records(records)
	return ui.new_state(records).records
end

ui.dedupe_records = ui.normalize_records

function ui.rebase(records, snapshot)
	if is_table(records) and records.by_id and records.order then
		if is_table(snapshot) and snapshot[1] ~= nil then
			for _, event in ipairs(snapshot) do
				ui.apply_event(records, event)
			end
		else
			ui.apply_event(records, snapshot)
		end
		records.records = state_records(records)
		return records, records.records
	end
	if is_table(snapshot) and (snapshot.rebase == true or snapshot.reconnect == true or snapshot.snapshot ~= nil) then
		return ui.reduce({ snapshot }, records)
	end
	local values = snapshot
	if is_table(snapshot) and snapshot.records ~= nil then
		values = snapshot.records
	elseif is_table(snapshot) and snapshot.agents ~= nil then
		values = snapshot.agents
	end
	return ui.normalize_records(values or records or {})
end

local function adapter_from(value)
	if is_table(value) and is_table(value.ui) then
		return value.ui
	end
	if is_table(value) and (value.row or value.text or value.icon) then
		return value
	end
	if type(_G) == "table" and is_table(_G.bootty) and is_table(_G.bootty.ui) then
		return _G.bootty.ui
	end
	return nil
end

local function invoke_function(function_value, owner, ...)
	if type(function_value) ~= "function" then
		return nil, false
	end
	local ok, result = pcall(function_value, ...)
	if ok then
		return result, true
	end
	if owner ~= nil then
		local retry_ok, retry_result = pcall(function_value, owner, ...)
		if retry_ok then
			return retry_result, true
		end
	end
	return nil, false
end

local function command_host(value)
	if is_table(value) and is_table(value.commands) then
		return value
	end
	if type(_G) == "table" and is_table(_G.bootty) and is_table(_G.bootty.commands) then
		return _G.bootty
	end
	return nil
end

function ui.invoke(command_id, arguments, host)
	local id = string_or_nil(command_id)
	if id == nil or id == "" then
		return nil, "missing command id"
	end
	local args = copy(arguments or {})
	local owner = command_host(host)
	local commands = owner and owner.commands or nil
	local result, invoked = nil, false
	if commands ~= nil then
		result, invoked = invoke_function(commands.invoke, commands, id, args)
	end
	if not invoked and is_table(host) then
		result, invoked = invoke_function(host.invoke, host, id, args)
	end
	if invoked then
		return result
	end
	-- A pure test/preview host has no side effect. Return an explicit invocation envelope so callers
	-- can inspect the command ID and opaque arguments without pretending the command ran.
	return { command_id = id, id = id, args = args, invoked = false }
end

local function merge(base, extra)
	local result = copy(base or {})
	if is_table(extra) then
		for key, value in pairs(extra) do
			result[key] = copy(value)
		end
	end
	return result
end

local function action_descriptor(command_id, base_args, host)
	local id = string_or_nil(command_id)
	local base = copy(base_args or {})
	local descriptor = {
		id = id,
		command_id = id,
		command = id,
		args = copy(base),
	}
	descriptor.invoke = function(extra, maybe_host)
		local supplied = extra
		local target = maybe_host or host
		if supplied == target then
			supplied = nil
		end
		if not is_table(supplied) then
			supplied = nil
		end
		return ui.invoke(id, merge(base, supplied), target)
	end
	return descriptor
end

function ui.action(command_id, agent_id, arguments, host)
	local args = merge({ agent_id = string_or_nil(agent_id) }, arguments)
	return action_descriptor(command_id, args, host)
end

function ui.actions(record, host)
	local id = record_id(record)
	if id == nil then
		return {}
	end
	local actions = {}
	for _, command_id in ipairs(ui.row_action_ids) do
		actions[#actions + 1] = ui.action(command_id, id, nil, host)
	end
	return actions
end

local function workflow_command(command_id, arguments, host)
	return ui.invoke(command_id, arguments, host)
end

ui.workflow = {}

function ui.workflow.list(filters, host)
	return workflow_command("agents.list", filters or {}, host)
end

function ui.workflow.start(provider, options, host)
	local arguments = merge(options, { provider = provider })
	return workflow_command("agents.start", arguments, host)
end

function ui.workflow.prompt(agent_id, prompt, mode, host)
	local arguments = { agent_id = string_or_nil(agent_id), prompt = prompt }
	if mode ~= nil then
		arguments.mode = mode
	end
	return workflow_command("agents.prompt", arguments, host)
end

function ui.workflow.steer(agent_id, text, host)
	return ui.workflow.prompt(agent_id, text, "steer", host)
end

function ui.workflow.wait(agent_id, timeout_ms, host)
	local arguments = { agent_id = string_or_nil(agent_id) }
	if timeout_ms ~= nil then
		arguments.timeout_ms = timeout_ms
	end
	return workflow_command("agents.wait", arguments, host)
end

function ui.workflow.cancel(agent_id, host)
	return workflow_command("agents.abort", { agent_id = string_or_nil(agent_id) }, host)
end

ui.workflow.abort = ui.workflow.cancel

-- Cleanup is explicit and ordered. It does not inspect lifecycle, terminal text, or process state;
-- callers choose this workflow after cancellation/teardown and every command receives the same
-- opaque agent_id. Without a host, invocation envelopes are returned for deterministic tests.
function ui.workflow.cleanup(agent_id, host)
	local commands = {
		{ "agents.view.clear", { agent_id = string_or_nil(agent_id) } },
		{ "agents.clear_authority", { agent_id = string_or_nil(agent_id) } },
		{ "agents.release", { agent_id = string_or_nil(agent_id) } },
	}
	local results = {}
	for _, item in ipairs(commands) do
		results[#results + 1] = workflow_command(item[1], item[2], host)
	end
	return results
end


function ui.workflow.integration_list(filters, host)
	return workflow_command("agents.integration.list", filters or {}, host)
end

function ui.workflow.integration_doctor(arguments, host)
	return workflow_command("agents.integration.doctor", arguments or {}, host)
end

function ui.workflow.integration_install(arguments, host)
	return workflow_command("agents.integration.install", arguments or {}, host)
end

function ui.workflow.integration_setup(arguments, host)
	return workflow_command("agents.integration.setup", arguments or {}, host)
end

function ui.workflow.integration_uninstall(arguments, host)
	return workflow_command("agents.integration.uninstall", arguments or {}, host)
end

function ui.workflow.manifest_list(arguments, host)
	return workflow_command("agents.manifest.list", arguments or {}, host)
end

function ui.workflow.manifest_reload(arguments, host)
	return workflow_command("agents.manifest.reload", arguments or {}, host)
end

ui.list = ui.workflow.list
ui.start = ui.workflow.start
ui.prompt = ui.workflow.prompt
ui.steer = ui.workflow.steer
ui.wait = ui.workflow.wait
ui.cancel = ui.workflow.cancel
ui.abort = ui.workflow.abort
ui.cleanup = ui.workflow.cleanup

function ui.subscribe(host, callback)
	local owner = host
	if not is_table(owner) and type(_G) == "table" then
		owner = _G.bootty
	end
	local events = owner and owner.events or nil
	if events == nil then
		return { topic = "agents.changed", subscribed = false }
	end
	local result, subscribed = invoke_function(events.subscribe, events, "agents.changed", callback)
	if subscribed then
		return result
	end
	return { topic = "agents.changed", subscribed = false }
end

local function primitive(adapter, name, value, options)
	if is_table(adapter) and type(adapter[name]) == "function" then
		local result, ok = invoke_function(adapter[name], adapter, value, options or {})
		if ok then
			return result
		end
	end
	if name == "icon" then
		return { type = "icon", icon = value, options = copy(options or {}) }
	end
	return { type = "text", text = value or "", options = copy(options or {}) }
end

local function row(adapter, payload)
	local rendered = nil
	if is_table(adapter) and type(adapter.row) == "function" then
		rendered = invoke_function(adapter.row, adapter, payload)
	end
	if not is_table(rendered) then
		rendered = copy(payload)
	else
		-- Generic host row primitives may intentionally discard extension metadata. Reattach only
		-- normalized facts/actions so click handlers can still pass opaque IDs to command.invoke.
		for key, value in pairs(payload) do
			if rendered[key] == nil then
				rendered[key] = copy(value)
			end
		end
	end
	return rendered
end

local function explicit_fact_pairs(record)
	local facts = {}
	for _, key in ipairs(FACT_FIELDS) do
		if record[key] ~= nil then
			facts[#facts + 1] = { key = key, value = copy(record[key]) }
		end
	end

	if is_table(record.terminal) then
		if record.terminal.terminal_id ~= nil then
			facts[#facts + 1] = { key = "terminal_id", value = copy(record.terminal.terminal_id) }
		end
		if record.terminal.process_id ~= nil then
			facts[#facts + 1] = { key = "process_id", value = copy(record.terminal.process_id) }
		end
	end

	if is_table(record.facts) then
		local keys = {}
		for key in pairs(record.facts) do
			keys[#keys + 1] = key
		end
		table.sort(keys, function(left, right)
			return tostring(left) < tostring(right)
		end)
		for _, key in ipairs(keys) do
			if record.facts[key] ~= nil then
				facts[#facts + 1] = {
					key = "facts." .. tostring(key),
					value = copy(record.facts[key]),
				}
			end
		end
	end
	if is_table(record.view) then
		facts[#facts + 1] = { key = "view", value = copy(record.view) }
	end
	return facts
end

ui.explicit_fact_pairs = explicit_fact_pairs

local function fact_text(value)
	if value == nil then
		return nil
	end
	if type(value) ~= "table" then
		return tostring(value)
	end
	local keys = {}
	for key in pairs(value) do
		keys[#keys + 1] = key
	end
	table.sort(keys, function(left, right)
		return tostring(left) < tostring(right)
	end)
	local parts = {}
	for _, key in ipairs(keys) do
		local child = value[key]
		if type(child) ~= "table" then
			parts[#parts + 1] = tostring(key) .. "=" .. tostring(child)
		end
	end
	return "{" .. table.concat(parts, ", ") .. "}"
end
local function preview_items(value)
	if is_table(value) and value.items ~= nil then
		return array_values(value.items)
	end
	if is_table(value) and value.records ~= nil then
		return array_values(value.records)
	end
	return array_values(value or {})
end

local function preview_row(item, kind, index, adapter, host, command_id)
	local explicit_id = is_table(item) and (item.id or item.provider or item.name)
	local key = tostring(explicit_id or index)
	local label = is_table(item) and (item.title or item.label or item.message or item.provider or item.name)
	if label == nil then
		label = key
	end
	local arguments = {}
	if is_table(item) then
		arguments = copy(item.arguments or item.args or {})
		if arguments.provider == nil and item.provider ~= nil then
			arguments.provider = copy(item.provider)
		end
	end
	local details = {}
	if is_table(item) then
		local keys = {}
		for field in pairs(item) do
			if field ~= "arguments" and field ~= "args" and field ~= "title" and field ~= "label" then
				keys[#keys + 1] = field
			end
		end
		table.sort(keys, function(left, right)
			return tostring(left) < tostring(right)
		end)
		for _, field in ipairs(keys) do
			details[#details + 1] = {
				key = tostring(field),
				label = tostring(field),
				value = fact_text(item[field]),
				fact = copy(item[field]),
			}
		end
	end
	local icon = "download"
	if kind == "diagnostic" then
		icon = "stethoscope"
	end
	return row(adapter, {
		key = kind .. ":" .. key,
		kind = kind,
		text = tostring(label),
		selectable = true,
		details = details,
		facts = copy(item),
		primitives = {
			primitive(adapter, "icon", icon, { size = 12 }),
			primitive(adapter, "text", tostring(label), { align = "left_center" }),
		},
		actions = { action_descriptor(command_id, arguments, host) },
	})
end

function ui.render_diagnostics(diagnostics, adapter, host)
	local resolved_adapter = adapter_from(adapter)
	local resolved_host = host
	if resolved_host == nil and is_table(adapter) and is_table(adapter.ui) then
		resolved_host = adapter
	end
	local rendered = {}
	for index, item in ipairs(preview_items(diagnostics)) do
		rendered[#rendered + 1] = preview_row(
			item,
			"diagnostic",
			index,
			resolved_adapter,
			resolved_host,
			"agents.integration.doctor"
		)
	end
	return rendered
end

function ui.render_install_preview(previews, adapter, host)
	local resolved_adapter = adapter_from(adapter)
	local resolved_host = host
	if resolved_host == nil and is_table(adapter) and is_table(adapter.ui) then
		resolved_host = adapter
	end
	local rendered = {}
	for index, item in ipairs(preview_items(previews)) do
		rendered[#rendered + 1] = preview_row(
			item,
			"install-preview",
			index,
			resolved_adapter,
			resolved_host,
			"agents.integration.install"
		)
	end
	return rendered
end

ui.render_diagnostics_preview = ui.render_diagnostics
ui.render_installation_preview = ui.render_install_preview


local function render_record(record, adapter, host)
	local id = record.agent_id
	local label = record.name
	if label == nil or label == "" then
		label = id
	end
	local facts = explicit_fact_pairs(record)
	local primitives = {
		primitive(adapter, "icon", "bot", { size = 12 }),
		primitive(adapter, "text", tostring(label), { align = "left_center" }),
	}
	-- State and provider are displayed only from normalized fields. `unknown` is the model's
	-- explicit sentinel, not a guess based on terminal/process presence.
	if record.lifecycle ~= nil then
		primitives[#primitives + 1] = primitive(adapter, "text", tostring(record.lifecycle), {
			align = "right_center",
			role = "lifecycle",
		})
	end
	if record.provider ~= nil then
		primitives[#primitives + 1] = primitive(adapter, "text", tostring(record.provider), {
			align = "right_center",
			role = "provider",
		})
	end
	local details = {}
	for _, fact in ipairs(facts) do
		details[#details + 1] = {
			key = fact.key,
			label = fact.key,
			value = fact_text(fact.value),
			fact = fact.value,
		}
	end
	local payload = {
		key = "agent:" .. id,
		kind = "agent",
		text = tostring(label),
		selectable = true,
		agent_id = id,
		provider = copy(record.provider),
		lifecycle = copy(record.lifecycle),
		facts = copy(record),
		details = details,
		primitives = primitives,
		actions = ui.actions(record, host),
	}
	return row(adapter, payload)
end

function ui.render(records, adapter, host)
	local source = records
	if is_table(source) and source.records ~= nil then
		source = source.records
	end
	local resolved_adapter = adapter_from(adapter)
	local resolved_host = host
	if resolved_host == nil and is_table(adapter) and is_table(adapter.ui) then
		resolved_host = adapter
	end
	local normalized = ui.normalize_records(source or {})
	local rendered = {}
	for _, record in ipairs(normalized) do
		rendered[#rendered + 1] = render_record(record, resolved_adapter, resolved_host)
	end
	return rendered
end

ui.render_records = ui.render
ui.render_list = ui.render
ui.actions_for_record = ui.actions
ui.command = ui.invoke

function ui.render_detail(record, adapter, host)
	local rows = ui.render({ record }, adapter, host)
	return rows[1]
end

function ui.invoke_action(action, arguments, host)
	if is_table(action) then
		return ui.invoke(action.command_id or action.id or action.command, merge(action.args, arguments), host)
	end
	return ui.invoke(action, arguments, host)
end

return ui
