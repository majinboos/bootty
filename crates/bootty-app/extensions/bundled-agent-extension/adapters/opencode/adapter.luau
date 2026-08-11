-- OpenCode adapter for the bundled agent extension.
-- This module only consumes schema-bearing plugin/server payloads. It never
-- derives agent state from terminal output, process names, or transcript data.
-- Keep this file valid Lua 5.4 as well as Luau: fixture checks run it without
-- the Bootty host.

local Adapter = {}

Adapter.id = "opencode"
Adapter.provider = "opencode"
Adapter.display_name = "OpenCode"
Adapter.version = "1.18.16"
Adapter.source_revision = "d041eee55c4b669f583fcbe0eb73e78d53393ae8"
Adapter.manifest_path = "manifest.json"

Adapter.manifest = {
    adapter_id = Adapter.id,
    provider = Adapter.provider,
    version = Adapter.version,
    source_revision = Adapter.source_revision,
    existing_protocol = "plugin",
    managed_protocol = "server_sse_sdk",
    schema_inspection_required = true,
}

local function is_table(value)
    return type(value) == "table"
end

local function is_id(value)
    local value_type = type(value)
    return (value_type == "string" and value ~= "") or value_type == "number"
end

local function copy_table(source)
    local result = {}
    if not is_table(source) then
        return result
    end
    for key, value in pairs(source) do
        result[key] = value
    end
    return result
end

local function first_present(source, keys)
    if not is_table(source) then
        return nil
    end
    for _, key in ipairs(keys) do
        local value = source[key]
        if value ~= nil then
            return value
        end
    end
    return nil
end

local function has_prefix(value, prefix)
    return type(value) == "string" and value:sub(1, #prefix) == prefix
end

local function sorted_keys(source)
    local keys = {}
    for key in pairs(source) do
        keys[#keys + 1] = key
    end
    table.sort(keys)
    return keys
end

local function diagnostic(code, message, details, raw)
    local result = {
        adapter = Adapter.id,
        authoritative = true,
        kind = code,
        diagnostic_kind = "diagnostic",
        code = code,
        message = message,
    }
    if details ~= nil then
        result.facts = details
        result.detail = details
    end
    if raw ~= nil then
        result.raw = raw
    end
    return result
end
local function success(event)
    return {
        ok = true,
        events = { event },
        diagnostics = {},
    }
end

local function failure(item, events)
    local result = {
        ok = false,
        events = events or {},
        diagnostics = { item },
        error = item,
    }
    return result
end

-- A small JSON decoder keeps fixture execution independent of the Bootty host.
-- The host may provide bootty.json.decode; this parser is the deterministic
-- fallback and deliberately rejects malformed input instead of recovering.
local Json = {}

local function json_error(state, message)
    error(message .. " at byte " .. tostring(state.index), 0)
end

local function json_skip_space(state)
    while true do
        local character = state.text:sub(state.index, state.index)
        if character == "" then
            return
        end
        if character == " " or character == "\t" or character == "\r" or character == "\n" then
            state.index = state.index + 1
        else
            return
        end
    end
end

local function json_parse_string(state)
    if state.text:sub(state.index, state.index) ~= '"' then
        json_error(state, "expected string")
    end
    state.index = state.index + 1
    local result = {}
    while state.index <= #state.text do
        local character = state.text:sub(state.index, state.index)
        state.index = state.index + 1
        if character == '"' then
            return table.concat(result)
        end
        if character == "\\" then
            local escape = state.text:sub(state.index, state.index)
            state.index = state.index + 1
            local escaped = {
                ['"'] = '"',
                ["\\"] = "\\",
                ["/"] = "/",
                ["b"] = "\b",
                ["f"] = "\f",
                ["n"] = "\n",
                ["r"] = "\r",
                ["t"] = "\t",
            }
            if escaped[escape] ~= nil then
                result[#result + 1] = escaped[escape]
            elseif escape == "u" then
                local hexadecimal = state.text:sub(state.index, state.index + 3)
                if not hexadecimal:match("^%x%x%x%x$") then
                    json_error(state, "invalid unicode escape")
                end
                local codepoint = tonumber(hexadecimal, 16)
                state.index = state.index + 4
                if codepoint < 0x80 then
                    result[#result + 1] = string.char(codepoint)
                elseif codepoint < 0x800 then
                    result[#result + 1] = string.char(
                        0xc0 + math.floor(codepoint / 0x40),
                        0x80 + (codepoint % 0x40)
                    )
                elseif codepoint < 0x10000 then
                    result[#result + 1] = string.char(
                        0xe0 + math.floor(codepoint / 0x1000),
                        0x80 + (math.floor(codepoint / 0x40) % 0x40),
                        0x80 + (codepoint % 0x40)
                    )
                else
                    result[#result + 1] = string.char(
                        0xf0 + math.floor(codepoint / 0x40000),
                        0x80 + (math.floor(codepoint / 0x1000) % 0x40),
                        0x80 + (math.floor(codepoint / 0x40) % 0x40),
                        0x80 + (codepoint % 0x40)
                    )
                end
            else
                json_error(state, "invalid escape")
            end
        elseif string.byte(character) < 0x20 then
            json_error(state, "control character in string")
        else
            result[#result + 1] = character
        end
    end
    json_error(state, "unterminated string")
end

local json_parse_value

local function json_parse_array(state)
    state.index = state.index + 1
    local result = {}
    json_skip_space(state)
    if state.text:sub(state.index, state.index) == "]" then
        state.index = state.index + 1
        return result
    end
    while true do
        result[#result + 1] = json_parse_value(state)
        json_skip_space(state)
        local character = state.text:sub(state.index, state.index)
        if character == "]" then
            state.index = state.index + 1
            return result
        end
        if character ~= "," then
            json_error(state, "expected comma or closing array bracket")
        end
        state.index = state.index + 1
        json_skip_space(state)
    end
end

local function json_parse_object(state)
    state.index = state.index + 1
    local result = {}
    json_skip_space(state)
    if state.text:sub(state.index, state.index) == "}" then
        state.index = state.index + 1
        return result
    end
    while true do
        if state.text:sub(state.index, state.index) ~= '"' then
            json_error(state, "expected object key")
        end
        local key = json_parse_string(state)
        json_skip_space(state)
        if state.text:sub(state.index, state.index) ~= ":" then
            json_error(state, "expected colon")
        end
        state.index = state.index + 1
        json_skip_space(state)
        result[key] = json_parse_value(state)
        json_skip_space(state)
        local character = state.text:sub(state.index, state.index)
        if character == "}" then
            state.index = state.index + 1
            return result
        end
        if character ~= "," then
            json_error(state, "expected comma or closing object brace")
        end
        state.index = state.index + 1
        json_skip_space(state)
    end
end

local function json_parse_number(state)
    local start = state.index
    local index = start
    local text = state.text
    local character = text:sub(index, index)
    if character == "-" then
        index = index + 1
        character = text:sub(index, index)
    end
    if character == "0" then
        index = index + 1
    elseif character:match("%d") then
        while text:sub(index, index):match("%d") do
            index = index + 1
        end
    else
        json_error(state, "invalid number")
    end
    if text:sub(index, index) == "." then
        index = index + 1
        if not text:sub(index, index):match("%d") then
            json_error(state, "invalid number fraction")
        end
        while text:sub(index, index):match("%d") do
            index = index + 1
        end
    end
    character = text:sub(index, index)
    if character == "e" or character == "E" then
        index = index + 1
        character = text:sub(index, index)
        if character == "+" or character == "-" then
            index = index + 1
        end
        if not text:sub(index, index):match("%d") then
            json_error(state, "invalid number exponent")
        end
        while text:sub(index, index):match("%d") do
            index = index + 1
        end
    end
    local token = text:sub(start, index - 1)
    state.index = index
    local number = tonumber(token)
    if number == nil then
        json_error(state, "number is not finite")
    end
    return number
end

json_parse_value = function(state)
    json_skip_space(state)
    local character = state.text:sub(state.index, state.index)
    if character == '"' then
        return json_parse_string(state)
    end
    if character == "{" then
        return json_parse_object(state)
    end
    if character == "[" then
        return json_parse_array(state)
    end
    if state.text:sub(state.index, state.index + 3) == "true" then
        state.index = state.index + 4
        return true
    end
    if state.text:sub(state.index, state.index + 4) == "false" then
        state.index = state.index + 5
        return false
    end
    if state.text:sub(state.index, state.index + 3) == "null" then
        state.index = state.index + 4
        return nil
    end
    return json_parse_number(state)
end

function Json.decode(text)
    if type(text) ~= "string" then
        return nil, "JSON input must be a string"
    end
    local state = { text = text, index = 1 }
    local ok, value_or_error = pcall(function()
        local value = json_parse_value(state)
        json_skip_space(state)
        if state.index <= #state.text then
            json_error(state, "trailing data")
        end
        return value
    end)
    if not ok then
        return nil, value_or_error
    end
    return value_or_error
end

local function decode_json(value)
    if type(value) ~= "string" then
        return value
    end
    local bootty_value = rawget(_G, "bootty")
    if is_table(bootty_value) and is_table(bootty_value.json) and type(bootty_value.json.decode) == "function" then
        local ok, decoded = pcall(bootty_value.json.decode, value)
        if ok and decoded ~= nil then
            return decoded
        end
    end
    return Json.decode(value)
end

local KNOWN_EVENTS = {
    ["server.connected"] = true,
    ["session.created"] = true,
    ["session.updated"] = true,
    ["session.deleted"] = true,
    ["session.status"] = true,
    ["session.idle"] = true,
    ["session.error"] = true,
    ["session.diff"] = true,
    ["session.compacted"] = true,
    ["message.updated"] = true,
    ["message.removed"] = true,
    ["message.part.updated"] = true,
    ["message.part.removed"] = true,
    ["message.part.delta"] = true,
    ["permission.asked"] = true,
    ["permission.ask"] = true,
    ["permission.replied"] = true,
    ["permission.v2.asked"] = true,
    ["permission.v2.replied"] = true,
    ["session.next.prompt.admitted"] = true,
    ["session.next.prompted"] = true,
    ["session.next.agent.switched"] = true,
    ["session.next.model.switched"] = true,
    ["session.next.moved"] = true,
    ["session.next.context.updated"] = true,
    ["session.next.synthetic"] = true,
    ["session.next.shell.started"] = true,
    ["session.next.shell.ended"] = true,
    ["session.next.step.started"] = true,
    ["session.next.step.ended"] = true,
    ["session.next.step.failed"] = true,
    ["session.next.text.started"] = true,
    ["session.next.text.delta"] = true,
    ["session.next.text.ended"] = true,
    ["session.next.reasoning.started"] = true,
    ["session.next.reasoning.delta"] = true,
    ["session.next.reasoning.ended"] = true,
    ["session.next.tool.input.started"] = true,
    ["session.next.tool.input.delta"] = true,
    ["session.next.tool.input.ended"] = true,
    ["session.next.tool.called"] = true,
    ["session.next.tool.progress"] = true,
    ["session.next.tool.success"] = true,
    ["session.next.tool.failed"] = true,
    ["session.next.retried"] = true,
    ["session.next.compaction.started"] = true,
    ["session.next.compaction.delta"] = true,
    ["session.next.compaction.ended"] = true,
    ["session.next.revert.staged"] = true,
    ["session.next.revert.cleared"] = true,
    ["session.next.revert.committed"] = true,
    ["tui.prompt.append"] = true,
    ["tui.command.execute"] = true,
    ["tui.toast.show"] = true,
    ["tui.session.select"] = true,
}

local V1_EVENTS = {
    ["session.created"] = true,
    ["session.updated"] = true,
    ["session.deleted"] = true,
    ["session.status"] = true,
    ["session.idle"] = true,
    ["session.error"] = true,
    ["session.diff"] = true,
    ["session.compacted"] = true,
    ["message.updated"] = true,
    ["message.removed"] = true,
    ["message.part.updated"] = true,
    ["message.part.removed"] = true,
    ["message.part.delta"] = true,
    ["permission.asked"] = true,
    ["permission.replied"] = true,
}

local function event_data(raw)
    if is_table(raw.data) then
        return raw.data
    end
    if is_table(raw.properties) then
        return raw.properties
    end
    return raw
end

local function event_id(raw, data)
    local candidate = first_present(raw, { "id", "event_id", "eventID" })
    if is_id(candidate) then
        return candidate
    end
    candidate = first_present(data, { "id", "event_id", "eventID" })
    if is_id(candidate) then
        return candidate
    end
    return nil
end

local function session_id(data)
    local candidate = first_present(data, { "sessionID", "session_id" })
    if is_id(candidate) then
        return candidate
    end
    return nil
end

local function native_id_for(event_type, raw, data)
    local candidate
    if event_type == "permission.asked" or event_type == "permission.ask" or event_type == "permission.v2.asked" then
        candidate = first_present(data, { "id", "permissionID", "permission_id" })
    elseif event_type == "permission.replied" or event_type == "permission.v2.replied" then
        candidate = first_present(data, { "requestID", "request_id", "id" })
    elseif has_prefix(event_type, "message.part.") then
        local part = data.part
        candidate = first_present(data, { "partID", "part_id" })
        if candidate == nil and is_table(part) then
            candidate = first_present(part, { "id", "partID", "part_id" })
        end
        if candidate == nil then
            candidate = first_present(data, { "messageID", "message_id" })
        end
    elseif event_type == "message.updated" then
        local info = data.info
        candidate = first_present(data, { "messageID", "message_id" })
        if candidate == nil and is_table(info) then
            candidate = first_present(info, { "id", "messageID", "message_id" })
        end
    elseif event_type == "message.removed" then
        candidate = first_present(data, { "messageID", "message_id" })
    elseif has_prefix(event_type, "session.next.tool.") then
        candidate = first_present(data, { "callID", "call_id" })
    elseif has_prefix(event_type, "session.next.") then
        candidate = first_present(data, { "messageID", "message_id", "assistantMessageID", "assistant_message_id" })
    end
    if not is_id(candidate) then
        candidate = session_id(data)
    end
    if not is_id(candidate) then
        candidate = event_id(raw, data)
    end
    return candidate
end

local function canonical_kind(event_type)
    local fixed = {
        ["server.connected"] = "server_connected",
        ["session.created"] = "session_started",
        ["session.updated"] = "session_updated",
        ["session.deleted"] = "session_deleted",
        ["session.status"] = "session_status",
        ["session.idle"] = "session_idle",
        ["session.error"] = "error",
        ["session.diff"] = "diff",
        ["session.compacted"] = "compaction",
        ["message.updated"] = "message_updated",
        ["message.removed"] = "message_removed",
        ["message.part.updated"] = "message_part_updated",
        ["message.part.removed"] = "message_part_removed",
        ["message.part.delta"] = "message_part_delta",
        ["permission.asked"] = "permission_requested",
        ["permission.ask"] = "permission_requested",
        ["permission.v2.asked"] = "permission_requested",
        ["permission.replied"] = "permission_resolved",
        ["permission.v2.replied"] = "permission_resolved",
        ["session.next.prompt.admitted"] = "prompt_admitted",
        ["session.next.prompted"] = "prompt_submitted",
        ["session.next.agent.switched"] = "agent_changed",
        ["session.next.model.switched"] = "model_changed",
        ["session.next.moved"] = "session_moved",
        ["session.next.context.updated"] = "context_updated",
        ["session.next.synthetic"] = "synthetic_message",
        ["session.next.shell.started"] = "shell_started",
        ["session.next.shell.ended"] = "shell_ended",
        ["session.next.step.started"] = "step_started",
        ["session.next.step.ended"] = "step_ended",
        ["session.next.step.failed"] = "step_failed",
        ["session.next.text.started"] = "text_started",
        ["session.next.text.delta"] = "text_delta",
        ["session.next.text.ended"] = "text_ended",
        ["session.next.reasoning.started"] = "reasoning_started",
        ["session.next.reasoning.delta"] = "reasoning_delta",
        ["session.next.reasoning.ended"] = "reasoning_ended",
        ["session.next.tool.input.started"] = "tool_input_started",
        ["session.next.tool.input.delta"] = "tool_input_delta",
        ["session.next.tool.input.ended"] = "tool_input_ended",
        ["session.next.tool.called"] = "tool_called",
        ["session.next.tool.progress"] = "tool_progress",
        ["session.next.tool.success"] = "tool_succeeded",
        ["session.next.tool.failed"] = "tool_failed",
        ["session.next.retried"] = "retry",
        ["session.next.compaction.started"] = "compaction_started",
        ["session.next.compaction.delta"] = "compaction_delta",
        ["session.next.compaction.ended"] = "compaction_ended",
        ["session.next.revert.staged"] = "revert_staged",
        ["session.next.revert.cleared"] = "revert_cleared",
        ["session.next.revert.committed"] = "revert_committed",
    }
    if fixed[event_type] ~= nil then
        return fixed[event_type]
    end
    if has_prefix(event_type, "tui.") then
        return "tui"
    end
    if has_prefix(event_type, "session.next.tool.") then
        return "tool"
    end
    if has_prefix(event_type, "session.next.compaction.") then
        return "compaction"
    end
    if has_prefix(event_type, "permission.") then
        return "permission"
    end
    return "unknown"
end

local function add_if_present(destination, source, source_key, destination_key)
    local value = source[source_key]
    if value ~= nil then
        destination[destination_key or source_key] = value
    end
end

local function normalized_facts(event_type, data, raw)
    local facts = {
        event_type = event_type,
    }
    local durable = raw.durable
    if is_table(durable) then
        add_if_present(facts, durable, "aggregateID", "aggregate_id")
        add_if_present(facts, durable, "seq", "sequence")
        add_if_present(facts, durable, "version")
    end
    add_if_present(facts, raw, "metadata")
    if event_type == "session.created" or event_type == "session.updated" or event_type == "session.deleted" then
        add_if_present(facts, data, "info", "session")
    elseif event_type == "session.status" then
        add_if_present(facts, data, "status")
    elseif event_type == "session.error" then
        add_if_present(facts, data, "error")
    elseif event_type == "session.diff" then
        add_if_present(facts, data, "diff")
    elseif event_type == "message.updated" then
        add_if_present(facts, data, "info", "message")
        local info = data.info
        if is_table(info) then
            add_if_present(facts, info, "tokens", "usage")
            add_if_present(facts, info, "cost")
            add_if_present(facts, info, "modelID", "model_id")
            add_if_present(facts, info, "providerID", "provider_id")
            add_if_present(facts, info, "variant")
        end
    elseif event_type == "message.removed" then
        add_if_present(facts, data, "messageID", "message_id")
    elseif event_type == "message.part.updated" then
        add_if_present(facts, data, "part")
        add_if_present(facts, data, "time")
        local part = data.part
        if is_table(part) then
            if part.type == "tool" then
                facts.tool = part
            elseif part.type == "reasoning" then
                facts.reasoning = part
            elseif part.type == "step-finish" then
                add_if_present(facts, part, "tokens", "usage")
                add_if_present(facts, part, "cost")
            end
        end
    elseif event_type == "message.part.removed" then
        add_if_present(facts, data, "messageID", "message_id")
        add_if_present(facts, data, "partID", "part_id")
    elseif event_type == "message.part.delta" then
        add_if_present(facts, data, "messageID", "message_id")
        add_if_present(facts, data, "partID", "part_id")
        add_if_present(facts, data, "field")
        add_if_present(facts, data, "delta")
    elseif event_type == "permission.asked" or event_type == "permission.ask" or event_type == "permission.v2.asked" then
        facts.permission = copy_table(data)
    elseif event_type == "permission.replied" or event_type == "permission.v2.replied" then
        facts.permission = copy_table(data)
    elseif has_prefix(event_type, "tui.") then
        facts.tui = copy_table(data)
    else
        for _, key in ipairs(sorted_keys(data)) do
            if key ~= "sessionID" and key ~= "session_id" then
                facts[key] = data[key]
            end
        end
    end
    return facts
end

local function sequence_for(raw)
    local durable = raw.durable
    if is_table(durable) and durable.seq ~= nil then
        return durable.seq
    end
    if raw.sequence ~= nil then
        return raw.sequence
    end
    return nil
end

local function schema_version_for(event_type, raw)
    if has_prefix(event_type, "session.next.") or event_type == "permission.v2.asked" or event_type == "permission.v2.replied" then
        return "v2"
    end
    if V1_EVENTS[event_type] then
        return "v1"
    end
    if raw.schema_version ~= nil then
        return raw.schema_version
    end
    return nil
end

local function validate_event_shape(raw, data, event_type)
    if not is_table(raw) then
        return false, diagnostic("adapter.malformed_payload", "OpenCode event must be an object", nil, raw)
    end
    if type(event_type) ~= "string" or event_type == "" then
        return false, diagnostic("adapter.malformed_payload", "OpenCode event type is missing", nil, raw)
    end
    local native_id = native_id_for(event_type, raw, data)
    if not is_id(native_id) then
        return false, diagnostic(
            "adapter.malformed_payload",
            "OpenCode event has no opaque native identity",
            { event_type = event_type },
            raw
        )
    end
    if event_type == "session.status" and not is_table(data.status) then
        return false, diagnostic(
            "adapter.malformed_payload",
            "session.status requires a status object",
            { event_type = event_type, native_id = native_id },
            raw
        )
    end
    if event_type == "permission.asked" or event_type == "permission.ask" or event_type == "permission.v2.asked" then
        if not is_id(first_present(data, { "id" })) or not is_id(session_id(data)) then
            return false, diagnostic(
                "adapter.malformed_payload",
                "permission.asked requires id and sessionID",
                { event_type = event_type },
                raw
            )
        end
    end
    return true, native_id
end

local function normalize_table(raw, mode, context)
    if not is_table(raw) then
        return nil, diagnostic("adapter.malformed_payload", "OpenCode payload must decode to an object", nil, raw)
    end
    local event_type = first_present(raw, { "type", "event_type", "eventType" })
    local data = event_data(raw)
    if type(event_type) ~= "string" and is_table(data) then
        event_type = first_present(data, { "type", "event_type", "eventType" })
    end
    local valid, native_or_diagnostic = validate_event_shape(raw, data, event_type)
    if not valid then
        return nil, native_or_diagnostic
    end
    local native_id = native_or_diagnostic
    local session = session_id(data)
    local event = {
        adapter = Adapter.id,
        kind = canonical_kind(event_type),
        native_id = native_id,
        facts = normalized_facts(event_type, data, raw),
        raw = raw,
    }
    if session ~= nil then
        event.adapter_session_id = session
    end
    local sequence = sequence_for(raw)
    if sequence ~= nil then
        event.sequence = sequence
    end
    if is_table(context) then
        if context.terminal_id ~= nil then
            event.terminal_id = context.terminal_id
        end
        if context.process_id ~= nil then
            event.process_id = context.process_id
        end
        if context.adapter_session_id ~= nil and event.adapter_session_id == nil then
            event.adapter_session_id = context.adapter_session_id
        end
    end
    local schema_version = schema_version_for(event_type, raw)
    if schema_version ~= nil then
        event.facts.schema_version = schema_version
    end
    if not KNOWN_EVENTS[event_type] then
        event.facts.payload = copy_table(data)
        return event, diagnostic(
            "adapter.unknown_event",
            "OpenCode event was not in the pinned manifest; forwarded as unknown",
            { event_type = event_type, mode = mode, schema_version = schema_version },
            raw
        )
    end
    return event, nil
end

function Adapter.decode(value)
    if type(value) == "table" then
        return value
    end
    return decode_json(value)
end

function Adapter.normalize(raw, mode, context)
    local decoded = Adapter.decode(raw)
    if decoded == nil then
        return failure(diagnostic("adapter.malformed_payload", "OpenCode payload could not be decoded", nil, raw))
    end
    local event, event_diagnostic = normalize_table(decoded, mode or "existing", context)
    if event_diagnostic ~= nil then
        return failure(event_diagnostic, event and { event } or {})
    end
    return success(event)
end

Adapter.parse = Adapter.normalize

function Adapter.parse_sse(frame, mode, context)
    if type(frame) ~= "string" then
        return failure(diagnostic("adapter.invalid_sse", "SSE frame must be text", nil, frame))
    end
    local data_lines = {}
    local event_name
    for raw_line in (frame .. "\n"):gmatch("([^\n]*)\n") do
        local line = raw_line
        if line:sub(-1) == "\r" then
            line = line:sub(1, -2)
        end
        if line:sub(1, 1) ~= ":" then
            local field, value = line:match("^([^:]+): ?(.*)$")
            if field == "data" then
                data_lines[#data_lines + 1] = value
            elseif field == "event" then
                event_name = value
            end
        end
    end
    if #data_lines == 0 then
        return failure(diagnostic("adapter.invalid_sse", "SSE frame has no data field", nil, frame))
    end
    local payload = table.concat(data_lines, "\n")
    local decoded = Adapter.decode(payload)
    if not is_table(decoded) then
        return failure(diagnostic("adapter.malformed_payload", "SSE data is not an event object", nil, payload))
    end
    if event_name ~= nil and decoded.type == nil then
        decoded.type = event_name
    end
    return Adapter.normalize(decoded, mode or "managed", context)
end

local function append_result(result, normalized)
    if normalized == nil then
        return
    end
    for _, event in ipairs(normalized.events or {}) do
        result.events[#result.events + 1] = event
    end
    for _, item in ipairs(normalized.diagnostics or {}) do
        result.diagnostics[#result.diagnostics + 1] = item
    end
end

function Adapter.forward(payloads, mode, context)
    local result = { ok = true, events = {}, diagnostics = {} }
    if type(payloads) == "string" then
        local decoded = Adapter.decode(payloads)
        if is_table(decoded) and decoded.type == nil then
            for _, payload in ipairs(decoded) do
                append_result(result, Adapter.normalize(payload, mode, context))
            end
        else
            append_result(result, Adapter.normalize(payloads, mode, context))
        end
    elseif is_table(payloads) and payloads.type ~= nil then
        append_result(result, Adapter.normalize(payloads, mode, context))
    elseif is_table(payloads) then
        for _, payload in ipairs(payloads) do
            append_result(result, Adapter.normalize(payload, mode, context))
        end
    else
        append_result(
            result,
            failure(diagnostic(
                "adapter.malformed_payload",
                "OpenCode forward input must be an event or array of events",
                nil,
                payloads
            ))
        )
    end
    result.ok = #result.diagnostics == 0
    if not result.ok then
        result.error = result.diagnostics[1]
    end
    return result
end

local function collect_event_names(value, names, seen)
    local value_type = type(value)
    if value_type == "string" then
        if KNOWN_EVENTS[value] then
            names[value] = true
        end
        return
    end
    if value_type ~= "table" or seen[value] then
        return
    end
    seen[value] = true
    for key, child in pairs(value) do
        collect_event_names(key, names, seen)
        collect_event_names(child, names, seen)
    end
end

local function schema_report(schema)
    local decoded = Adapter.decode(schema)
    if decoded == nil then
        return {
            events = {},
            diagnostics = {
                diagnostic("adapter.schema_unavailable", "OpenCode runtime schema could not be decoded", nil, schema),
            },
        }
    end
    local names = {}
    collect_event_names(decoded, names, {})
    local events = {}
    for name in pairs(names) do
        events[#events + 1] = name
    end
    table.sort(events)
    return { events = events, diagnostics = {} }
end

function Adapter.inspect_schema(schema)
    local report = schema_report(schema)
    return report.events
end

function Adapter.inspect_schema_report(schema)
    return schema_report(schema)
end

function Adapter.is_known_event(event_type)
    return KNOWN_EVENTS[event_type] == true
end

function Adapter.capabilities(schema)
    local report = schema_report(schema)
    local present = {}
    for _, event_type in ipairs(report.events) do
        present[event_type] = true
    end
    local capabilities = {
        plugin_event = present["session.status"] or present["message.part.updated"] or false,
        permissions_v1 = present["permission.asked"] and present["permission.replied"] or false,
        permissions_v2 = present["permission.v2.asked"] and present["permission.v2.replied"] or false,
        session_status = present["session.status"] or false,
        session_idle = present["session.idle"] or false,
        compaction = present["session.compacted"] or present["session.next.compaction.started"] or false,
        diff = present["session.diff"] or false,
        tool = present["session.next.tool.called"] or false,
        tui = present["tui.command.execute"] or present["tui.prompt.append"] or false,
        cursor_replay = false,
        wait = false,
        compact = false,
    }
    capabilities.events = report.events
    capabilities.diagnostics = report.diagnostics
    return capabilities
end

function Adapter.diagnostic(code, message, details, raw)
    return diagnostic(code, message, details, raw)
end

return Adapter
