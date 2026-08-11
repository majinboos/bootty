--!strict
-- Pi 0.84.1 adapter.
-- This module uses ordinary Lua 5.4 syntax and consumes only authoritative
-- Pi extension-hook and stdio JSONL RPC objects.

local M = {}
M.provider = "pi"
M.adapter = "pi"
M.id = "pi"
M.version = "0.84.1"
M.protocol = {
    existing = { kind = "extension_hook", source = "adapters/pi/extension.ts", transport = "in_process" },
    managed = {
        executable = "pi", arguments = { "--mode", "rpc" },
        transport = "stdio_jsonl", framing = "lf_jsonl",
    },
}

M.capabilities = {
    lifecycle = { "session_start", "session_info_changed", "session_before_switch", "session_before_fork", "session_shutdown", "agent_start", "agent_end", "agent_settled", "turn_start", "turn_end" },
    message = { "message_start", "message_update", "message_end" },
    turn = { "turn_start", "turn_end", "agent_start", "agent_end", "agent_settled" },
    tool = { "tool_execution_start", "tool_execution_update", "tool_execution_end", "tool_call", "tool_result" },
    model = { "model_select", "thinking_level_select", "thinking_level_changed" },
    reasoning = { "thinking_level_select", "thinking_level_changed" },
    usage = { "message_start", "message_update", "message_end", "get_session_stats" },
    cost = { "message_start", "message_update", "message_end", "get_session_stats" },
    compaction = { "session_before_compact", "session_compact", "compaction_start", "compaction_end" },
    session_switch = { "session_before_switch", "new_session", "switch_session" },
    session_fork = { "session_before_fork", "fork", "clone", "get_fork_messages", "get_tree", "get_entries" },
    shutdown = { "session_shutdown", "EOF", "SIGHUP", "SIGTERM" },
    settled = { "agent_settled" },
}

M.fidelity_limits = {
    "agent_end is intermediate; agent_settled is the authoritative no-continuation boundary",
    "RPC message_update records contain deltas only; message_end.message is the final message",
    "tool updates and completions may interleave; correlate them by opaque toolCallId",
    "Pi has no dedicated extension event for permission decisions, rate limits, quota, or billing",
    "usage, reasoning, and cost remain absent when Pi or its provider omits them",
    "session switch and fork hooks run before replacement and can cancel the operation",
    "RPC has no shutdown command; EOF, SIGHUP, or SIGTERM terminate the worker",
    "prompt responses acknowledge acceptance or queueing, not completion",
    "terminal output, process names, screen state, and transcript tails are not facts",
}

local hook_events = {
    session_start = true, session_info_changed = true, session_before_switch = true,
    session_before_fork = true, session_before_compact = true, session_compact = true,
    session_shutdown = true, before_agent_start = true, agent_start = true, agent_end = true,
    agent_settled = true, turn_start = true, turn_end = true, message_start = true,
    message_update = true, message_end = true, tool_execution_start = true,
    tool_execution_update = true, tool_execution_end = true, model_select = true,
    thinking_level_select = true, tool_call = true, tool_result = true, input = true,
}
local rpc_events = {
    agent_start = true, agent_end = true, agent_settled = true, turn_start = true,
    turn_end = true, message_start = true, message_update = true, message_end = true,
    tool_execution_start = true, tool_execution_update = true, tool_execution_end = true,
    queue_update = true, compaction_start = true, compaction_end = true,
    entry_appended = true, session_info_changed = true, thinking_level_changed = true,
    auto_retry_start = true, auto_retry_end = true, summarization_retry_scheduled = true,
    summarization_retry_attempt_start = true, summarization_retry_finished = true,
    extension_error = true,
}
local rpc_commands = {
    prompt = true, steer = true, follow_up = true, abort = true, get_state = true,
    new_session = true, switch_session = true, fork = true, clone = true,
    get_fork_messages = true, get_entries = true, get_tree = true, get_messages = true,
    set_session_name = true, set_model = true, cycle_model = true,
    get_available_models = true, set_thinking_level = true, cycle_thinking_level = true,
    get_available_thinking_levels = true, set_steering_mode = true,
    set_follow_up_mode = true, compact = true, set_auto_compaction = true,
    get_session_stats = true, set_auto_retry = true, abort_retry = true,
    bash = true, abort_bash = true, export_html = true,
}

local function is_table(value)
    return type(value) == "table"
end

local function is_object(value)
    if not is_table(value) then
        return false
    end
    for key, _ in pairs(value) do
        if type(key) ~= "string" then
            return false
        end
    end
    return true
end

local function copy(value, seen)
    if not is_table(value) then
        return value
    end
    local memo = seen or {}
    if memo[value] ~= nil then
        return memo[value]
    end
    local result = {}
    memo[value] = result
    for key, item in pairs(value) do
        result[copy(key, memo)] = copy(item, memo)
    end
    return result
end
M.copy = copy
M.methods = {
    "start", "read", "focus", "prompt", "send_text", "send_keys", "wait",
    "pause", "resume", "retry", "abort", "install", "uninstall", "setup", "doctor",
}
M.manifest = {
    id = M.id,
    provider = M.provider,
    version = M.version,
    protocol = M.protocol,
    capabilities = M.capabilities,
    methods = M.methods,
}

local function diagnostic(code, message, details)
    local result = {
        ok = false,
        adapter = M.adapter,
        kind = "diagnostic",
        code = code,
        message = message,
        diagnostic = { code = code, message = message },
    }
    if is_table(details) then
        result.facts = copy(details)
        for key, value in pairs(details) do
            result[key] = copy(value)
            result.diagnostic[key] = copy(value)
        end
    end
    return result
end
M.diagnostic = diagnostic

local function pick(object, keys)
    if not is_table(object) then
        return nil, nil
    end
    for index = 1, #keys do
        local key = keys[index]
        if object[key] ~= nil then
            return object[key], key
        end
    end
    return nil, nil
end
local function first_value(objects, keys)
    for index = 1, #objects do
        local value, key = pick(objects[index], keys)
        if value ~= nil then
            return value, key
        end
    end
    return nil, nil
end

local function valid_id(value, field)
    if value == nil then
        return nil, nil
    end
    local value_type = type(value)
    if value_type ~= "string" and value_type ~= "number" then
        return nil, diagnostic("malformed_id", field .. " must be an opaque string or number", { field = field })
    end
    if value_type == "string" and value == "" then
        return nil, diagnostic("malformed_id", field .. " must not be empty", { field = field })
    end
    if value_type == "number" and (value ~= value or value == math.huge or value == -math.huge) then
        return nil, diagnostic("malformed_id", field .. " must be finite", { field = field })
    end
    return value, nil
end

local function explicit_source(root, body, supplied, name)
    local source = supplied
    if source == nil then
        local protocol = first_value({ root, body }, { "protocol", "origin" })
        if protocol ~= nil then
            source = protocol
        else
            local marker = pick(root, { "source" })
            if marker == "extension" or marker == "typescript_extension" or marker == "hook"
                or marker == "managed" or marker == "rpc" or marker == "rpc_event" then
                source = marker
            elseif marker ~= nil and name ~= "input" and name ~= "model_select" then
                return nil, diagnostic("malformed_source", "Pi event source must be hook or rpc", { source = marker })
            end
        end
    end
    if source == "extension" or source == "typescript_extension" or source == "hook" then
        return "hook", nil
    end
    if source == "managed" or source == "rpc" or source == "rpc_event" then
        return "rpc", nil
    end
    if source ~= nil then
        return nil, diagnostic("malformed_source", "Pi event source must be hook or rpc", { source = source })
    end
    return nil, nil
end

local function unwrap(value)
    local body = value
    if type(value.event) == "table" then
        body = value.event
    elseif type(value.payload) == "table" then
        body = value.payload
    elseif (value.type == "hook_event" or value.type == "rpc_event" or value.type == "hook") and type(value.data) == "table" then
        body = value.data
    end
    return value, body
end

local function event_type(root, body)
    local value = pick(root, { "event", "event_name", "eventName", "hook_event" })
    if type(value) == "string" and value ~= "" then
        return value
    end
    value = pick(body, { "event", "event_name", "eventName", "hook_event" })
    if type(value) == "string" and value ~= "" then
        return value
    end
    value = pick(root, { "type" })
    if type(value) == "string" and value ~= "" and value ~= "hook" and value ~= "hook_event" and value ~= "rpc_event" then
        return value
    end
    value = pick(body, { "type" })
    if type(value) == "string" and value ~= "" and value ~= "hook" and value ~= "hook_event" and value ~= "rpc_event" then
        return value
    end
    return nil
end

local function source_for_type(source, name)
    if source ~= nil then
        return source
    end
    if name == "response" or rpc_events[name] then
        return "rpc"
    end
    if hook_events[name] then
        return "hook"
    end
    return nil
end

local function nested_message_id(message)
    if not is_table(message) then
        return nil, nil
    end
    return valid_id(pick(message, { "id", "messageId", "message_id" }), "message.id")
end

local function model_identifier(model)
    if not is_table(model) then
        return nil, nil
    end
    local provider = pick(model, { "provider" })
    local identifier = pick(model, { "id", "modelId", "model_id" })
    local _, provider_error = valid_id(provider, "model.provider")
    if provider_error then
        return nil, provider_error
    end
    local _, identifier_error = valid_id(identifier, "model.id")
    if identifier_error then
        return nil, identifier_error
    end
    if identifier ~= nil then
        return identifier, nil
    end
    return nil, nil
end

local function kind_for(name, command, success)
    if name == "response" then
        if success == false then
            return "error"
        elseif command == "get_state" then
            return "state"
        elseif command == "get_session_stats" then
            return "usage"
        elseif command == "set_model" or command == "cycle_model" then
            return "model_changed"
        elseif command == "set_thinking_level" or command == "cycle_thinking_level" then
            return "reasoning_changed"
        elseif command == "compact" then
            return "compaction"
        elseif command == "new_session" or command == "switch_session" then
            return "session_switched"
        elseif command == "fork" or command == "clone" then
            return "session_forked"
        end
        return "rpc_response"
    end
    local kinds = {
        session_start = "session_started", session_info_changed = "state",
        session_before_switch = "session_switched", session_before_fork = "session_forked",
        session_before_compact = "compaction", session_compact = "compaction",
        session_shutdown = "session_ended", before_agent_start = "prompt_submitted",
        input = "prompt_submitted", agent_start = "turn_started", agent_end = "turn_completed",
        agent_settled = "agent_settled", turn_start = "turn_started", turn_end = "turn_completed",
        message_start = "message_started", message_update = "message_updated",
        message_end = "message_completed", tool_execution_start = "tool_started",
        tool_execution_update = "tool_updated", tool_execution_end = "tool_completed",
        tool_call = "tool_call", tool_result = "tool_result",
        model_select = "model_changed", thinking_level_select = "reasoning_changed",
        thinking_level_changed = "reasoning_changed", queue_update = "state",
        compaction_start = "compaction", compaction_end = "compaction",
        entry_appended = "state", auto_retry_start = "state", auto_retry_end = "state",
        summarization_retry_scheduled = "state", summarization_retry_attempt_start = "state",
        summarization_retry_finished = "state", extension_error = "error",
    }
    return kinds[name]
end

local function add_fact(facts, key, value)
    if value ~= nil then
        facts[key] = copy(value)
    end
end

local fact_fields = {
    { { "reason" }, "reason" },
    { { "previousSessionFile", "previous_session_file" }, "previous_session_file" },
    { { "targetSessionFile", "target_session_file" }, "target_session_file" },
    { { "position" }, "position" },
    { { "entryId", "entry_id" }, "entry_id" },
    { { "leafId", "leaf_id" }, "leaf_id" },
    { { "parentId", "parent_id" }, "parent_id" },
    { { "sessionFile", "session_file" }, "session_file" },
    { { "sessionName", "session_name" }, "session_name" },
    { { "name" }, "name" },
    { { "cwd" }, "cwd" }, { { "mode" }, "mode" }, { { "signal" }, "signal" },
    { { "prompt" }, "prompt" }, { { "text" }, "text" }, { { "images" }, "images" },
    { { "streamingBehavior", "streaming_behavior" }, "streaming_behavior" },
    { { "inputSource", "input_source" }, "input_source" }, { { "turnIndex", "turn_index" }, "turn_index" },
    { { "timestamp" }, "timestamp" }, { { "timestampMs", "timestamp_ms" }, "timestamp_ms" },
    { { "level" }, "level" }, { { "thinkingLevel", "thinking_level" }, "thinking_level" },
    { { "previousLevel", "previous_level" }, "previous_level" },
    { { "previousModel", "previous_model" }, "previous_model" }, { { "model" }, "model" },
    { { "args" }, "args" }, { { "partialResult", "partial_result" }, "partial_result" },
    { { "result" }, "result" }, { { "isError", "is_error" }, "is_error" },
    { { "input" }, "input" }, { { "content" }, "content" }, { { "details" }, "details" },
    { { "usage" }, "usage" }, { { "cost" }, "cost" },
    { { "contextUsage", "context_usage" }, "context_usage" },
    { { "preparation" }, "preparation" }, { { "branchEntries", "branch_entries" }, "branch_entries" },
    { { "customInstructions", "custom_instructions" }, "custom_instructions" },
    { { "willRetry", "will_retry" }, "will_retry" },
    { { "compactionEntry", "compaction_entry" }, "compaction_entry" },
    { { "fromExtension", "from_extension" }, "from_extension" },
    { { "messages" }, "messages" }, { { "toolResults", "tool_results" }, "tool_results" },
    { { "assistantMessageEvent", "assistant_message_event" }, "assistant_message_event" },
    { { "steering", "steer" }, "steering" }, { { "followUp", "follow_up" }, "follow_up" },
    { { "isStreaming", "is_streaming" }, "is_streaming" }, { { "isCompacting", "is_compacting" }, "is_compacting" },
    { { "steeringMode", "steering_mode" }, "steering_mode" }, { { "followUpMode", "follow_up_mode" }, "follow_up_mode" },
    { { "messageCount", "message_count" }, "message_count" },
    { { "pendingMessageCount", "pending_message_count" }, "pending_message_count" },
    { { "userMessageCount", "user_message_count" }, "user_message_count" },
    { { "assistantMessageCount", "assistant_message_count" }, "assistant_message_count" },
    { { "toolCallCount", "tool_call_count" }, "tool_call_count" },
    { { "totalMessageCount", "total_message_count" }, "total_message_count" },
    { { "tokens" }, "tokens" }, { { "error" }, "error" },
    { { "extensionPath", "extension_path" }, "extension_path" },
}

local function normalize(value, source_hint)
    if not is_object(value) then
        return nil, diagnostic("malformed_event", "Pi event must be an object")
    end
    local root, body = unwrap(value)
    if not is_object(body) then
        return nil, diagnostic("malformed_event", "Pi event payload must be an object")
    end
    local name = event_type(root, body)
    if name == nil then
        return nil, diagnostic("malformed_event", "Pi event type is required")
    end
    local source, source_error = explicit_source(root, body, source_hint, name)
    if source_error then
        return nil, source_error
    end

    local command, success, response_data
    if name == "response" then
        command = pick(root, { "command" })
        if type(command) ~= "string" or command == "" then
            return nil, diagnostic("malformed_response", "Pi RPC response command is required")
        end
        success = pick(root, { "success" })
        if type(success) ~= "boolean" then
            return nil, diagnostic("malformed_response", "Pi RPC response success must be boolean")
        end
        response_data = pick(root, { "data" })
        body = response_data or {}
        source = source or "rpc"
    else
        source = source_for_type(source, name)
        if source == nil then
            return nil, diagnostic("unsupported_event", "Pi event is not a known hook or RPC event", { type = name })
        end
        if not hook_events[name] and not rpc_events[name] then
            return nil, diagnostic("unsupported_event", "Pi event is not supported", { type = name })
        end
    end

    local context = first_value({ body, root }, { "context" })
    if not is_object(context) then
        context = {}
    end
    local objects = { body, root, context }
    local session_value = first_value(objects, { "adapter_session_id", "adapterSessionId", "session_id", "sessionId", "native_session_id", "nativeSessionId" })
    if session_value == nil then
        local session = first_value(objects, { "session" })
        if type(session) == "string" or type(session) == "number" then
            session_value = session
        elseif is_object(session) then
            session_value = pick(session, { "id", "sessionId", "session_id" })
        end
    end
    local session_id, id_error = valid_id(session_value, "session_id")
    if id_error then
        return nil, id_error
    end

    local event_id, event_error = valid_id(first_value({ body, root }, { "event_id", "eventId", "native_id", "nativeId" }), "event_id")
    if event_error then
        return nil, event_error
    end
    local request_id
    if name == "response" then
        request_id, event_error = valid_id(pick(root, { "id" }), "response.id")
        if event_error then
            return nil, event_error
        end
    end
    local tool_id, tool_error = valid_id(first_value({ body, root }, { "toolCallId", "tool_call_id" }), "toolCallId")
    if tool_error then
        return nil, tool_error
    end
    local entry_id, entry_error = valid_id(first_value({ body, root }, { "entryId", "entry_id" }), "entryId")
    if entry_error then
        return nil, entry_error
    end
    local message_value = first_value({ body, root }, { "message" })
    local message_id, message_error = nested_message_id(message_value)
    if message_error then
        return nil, message_error
    end
    local model_value = first_value({ body, root }, { "model" })
    local model_id, model_error = model_identifier(model_value)
    if model_error then
        return nil, model_error
    end
    local sequence_value = first_value({ body, root, context }, { "sequence", "seq" })
    if sequence_value ~= nil and (type(sequence_value) ~= "number"
        or sequence_value ~= sequence_value
        or sequence_value == math.huge
        or sequence_value == -math.huge) then
        return nil, diagnostic("malformed_sequence", "Pi sequence must be a finite number")
    end
    local terminal_id, terminal_error = valid_id(first_value({ body, root, context }, { "terminal_id", "terminalId" }), "terminal_id")
    if terminal_error then
        return nil, terminal_error
    end
    local process_id, process_error = valid_id(first_value({ body, root, context }, { "process_id", "processId" }), "process_id")
    if process_error then
        return nil, process_error
    end

    local native_id = event_id or tool_id or message_id or entry_id or request_id or session_id
    if native_id == nil then
        return nil, diagnostic("missing_native_id", "Pi event has no authoritative opaque id", { type = name })
    end
    local kind = kind_for(name, command, success)
    if kind == nil then
        return nil, diagnostic("unsupported_event", "Pi event has no normalized kind", { type = name })
    end

    local facts = {}
    add_fact(facts, "source", source)
    add_fact(facts, "native_event", name)
    add_fact(facts, "event_id", event_id)
    add_fact(facts, "request_id", request_id)
    add_fact(facts, "session_id", session_id)
    add_fact(facts, "tool_call_id", tool_id)
    add_fact(facts, "entry_id", entry_id)
    add_fact(facts, "message_id", message_id)
    add_fact(facts, "model_id", model_id)
    for index = 1, #fact_fields do
        local descriptor = fact_fields[index]
        add_fact(facts, descriptor[2], first_value({ body, root }, descriptor[1]))
    end
    if name == "input" then
        add_fact(facts, "input_source", first_value({ body, root }, { "inputSource", "input_source", "source" }))
    elseif name == "model_select" then
        add_fact(facts, "selection_source", first_value({ body, root }, { "selectionSource", "selection_source", "source" }))
    end
    if name == "response" then
        add_fact(facts, "command", command)
        add_fact(facts, "success", success)
        add_fact(facts, "data", response_data)
        if success == false then
            add_fact(facts, "error", pick(root, { "error" }))
        end
        if is_object(response_data) then
            add_fact(facts, "cancelled", pick(response_data, { "cancelled" }))
            if command == "set_model" or command == "cycle_model" then
                add_fact(facts, "model", response_data)
                local response_model_id = model_identifier(response_data)
                add_fact(facts, "model_id", response_model_id)
            elseif command == "set_thinking_level" or command == "cycle_thinking_level" then
                add_fact(facts, "level", pick(response_data, { "level", "thinkingLevel", "thinking_level" }))
            end
        end
    end
    if message_value ~= nil then
        add_fact(facts, "message", message_value)
        if is_object(message_value) then
            local message_usage = pick(message_value, { "usage" })
            local message_cost = pick(message_value, { "cost" })
            if facts.usage == nil then
                add_fact(facts, "usage", message_usage)
            end
            if facts.cost == nil then
                add_fact(facts, "cost", message_cost)
            end
            if facts.cost == nil and is_object(message_usage) then
                add_fact(facts, "cost", pick(message_usage, { "cost" }))
            end
        end
    end
    if facts.usage ~= nil and facts.cost == nil and is_object(facts.usage) then
        add_fact(facts, "cost", pick(facts.usage, { "cost" }))
    end

    local tool = {}
    add_fact(tool, "id", tool_id)
    add_fact(tool, "name", first_value({ body, root }, { "toolName", "tool_name" }))
    add_fact(tool, "args", first_value({ body, root }, { "args" }))
    add_fact(tool, "input", first_value({ body, root }, { "input" }))
    add_fact(tool, "partial_result", first_value({ body, root }, { "partialResult", "partial_result" }))
    add_fact(tool, "result", first_value({ body, root }, { "result" }))
    add_fact(tool, "is_error", first_value({ body, root }, { "isError", "is_error" }))
    if next(tool) ~= nil then
        facts.tool = tool
    end

    local event = {
        adapter = M.adapter,
        kind = kind,
        native_id = copy(native_id),
        facts = facts,
        raw = copy(value),
    }
    if session_id ~= nil then event.adapter_session_id = copy(session_id) end
    if sequence_value ~= nil then event.sequence = sequence_value end
    if terminal_id ~= nil then event.terminal_id = copy(terminal_id) end
    if process_id ~= nil then event.process_id = copy(process_id) end
    return event, nil
end

local function decode(value)
    if is_table(value) then
        return value, nil
    end
    if type(value) ~= "string" then
        return nil, diagnostic("malformed_event", "Pi event must be a table or JSON string")
    end
    if type(bootty) == "table" and type(bootty.json) == "table" and type(bootty.json.decode) == "function" then
        local ok, result = pcall(bootty.json.decode, value)
        if ok and is_table(result) then
            return result, nil
        end
    end
    return nil, diagnostic("malformed_json", "Pi JSON input could not be decoded as an object", { input = value })
end

M.decode = decode

local function parse(value, source)
    local decoded, decode_error = decode(value)
    if decoded == nil then
        return decode_error
    end
    local mode = source
    if is_object(source) then
        mode = source.mode or source.source
    end
    if mode == "hooks" or mode == "existing" or mode == "interactive" then
        mode = "hook"
    elseif mode == "managed" or mode == "stream" or mode == "rpc_event" then
        mode = "rpc"
    end
    local event, parse_error = normalize(decoded, mode)
    if event == nil then
        return parse_error
    end
    return event
end

local function normalized_result(value, source)
    local result = parse(value, source)
    if result ~= nil and result.ok == false then
        return { ok = false, events = {}, diagnostics = { result }, diagnostic = result, error = result }
    end
    local wrapped = { ok = true, events = { result }, diagnostics = {}, event = result }
    if result ~= nil then
        for key, item in pairs(result) do
            wrapped[key] = copy(item)
        end
    end
    return wrapped
end

M.parse = parse
M.parse_event = parse
M.read = parse
M.normalize_event = normalize
M.normalize = normalized_result
function M.parse_existing(value)
    return parse(value, "hook")
end
function M.parse_managed(value)
    return parse(value, "rpc")
end
M.parse_hook = M.parse_existing
M.parse_rpc = M.parse_managed
M.forward_hook = M.parse_existing
M.forward_rpc = M.parse_managed
function M.forward(value, sink, source)
    if sink ~= nil and type(sink) ~= "function" then
        source = sink
        sink = nil
    end
    local event = parse(value, source)
    if event == nil or event.ok == false then
        return { accepted = false, diagnostic = event }
    end
    if sink == nil then
        return { accepted = true, event = event }
    end
    local ok, result = pcall(sink, event)
    if not ok then
        return { accepted = false, event = event, diagnostic = diagnostic("forward_failed", "Pi event sink rejected the event", { error = result }) }
    end
    if result == false then
        return { accepted = false, event = event, diagnostic = diagnostic("forward_rejected", "Pi event sink returned false") }
    end
    return { accepted = true, event = event }
end
M.forward_event = M.forward
M.ingest = M.forward

function M.make_forwarder(sink, source)
    return function(value)
        return M.forward(value, sink, source)
    end
end

local function command_error(code, message, details)
    return nil, diagnostic(code, message, details)
end

local function make_command(command_name, fields)
    if type(command_name) ~= "string" or command_name == "" or not rpc_commands[command_name] then
        return command_error("unsupported_command", "Pi RPC command is not supported", { command = command_name })
    end
    if fields ~= nil and not is_object(fields) then
        return command_error("malformed_command", "Pi RPC command fields must be an object")
    end
    local result = { type = command_name }
    if fields ~= nil then
        for key, value in pairs(fields) do
            if key ~= "type" then result[key] = copy(value) end
        end
    end
    return result, nil
end
M.command = make_command
M.rpc_command = make_command

local function options_copy(options)
    if options == nil then return {} end
    if not is_object(options) then return nil, diagnostic("malformed_command", "Pi command options must be an object") end
    return copy(options), nil
end

local function text_command(command_name, message, options)
    if type(message) ~= "string" then return command_error("malformed_command", "Pi message must be a string") end
    local fields, options_error = options_copy(options)
    if options_error then return nil, options_error end
    if fields.streamingBehavior ~= nil and fields.streamingBehavior ~= "steer" and fields.streamingBehavior ~= "followUp" then
        return command_error("malformed_command", "Pi streamingBehavior must be steer or followUp")
    end
    fields.message = message
    return make_command(command_name, fields)
end
function M.prompt(message, options) return text_command("prompt", message, options) end
function M.steer(message, options)
    local fields, err = options_copy(options)
    if err then return nil, err end
    if type(message) ~= "string" then return command_error("malformed_command", "Pi steer message must be a string") end
    fields.message = message
    return make_command("steer", fields)
end
function M.follow_up(message, options)
    local fields, err = options_copy(options)
    if err then return nil, err end
    if type(message) ~= "string" then return command_error("malformed_command", "Pi follow_up message must be a string") end
    fields.message = message
    return make_command("follow_up", fields)
end
M.followUp = M.follow_up
function M.abort(options) return make_command("abort", options) end
function M.get_state(options) return make_command("get_state", options) end
M.state = M.get_state

function M.new_session(parent_session, options)
    local fields = options
    if is_object(parent_session) then
        fields = parent_session
    else
        local copied, err = options_copy(options)
        if err then return nil, err end
        fields = copied
        if parent_session ~= nil then
            if type(parent_session) ~= "string" then return command_error("malformed_command", "Pi parentSession must be a string") end
            fields.parentSession = parent_session
        end
    end
    return make_command("new_session", fields)
end
function M.switch_session(session_path, options)
    if type(session_path) ~= "string" or session_path == "" then return command_error("malformed_command", "Pi sessionPath must be a non-empty string") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.sessionPath = session_path
    return make_command("switch_session", fields)
end
function M.fork(entry_id, options)
    if type(entry_id) ~= "string" or entry_id == "" then return command_error("malformed_command", "Pi entryId must be a non-empty string") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.entryId = entry_id
    return make_command("fork", fields)
end
function M.clone(options) return make_command("clone", options) end
function M.get_entries(since, options)
    local fields = options
    if is_object(since) then
        fields = since
    else
        local copied, err = options_copy(options)
        if err then return nil, err end
        fields = copied
        if since ~= nil then
            if type(since) ~= "string" or since == "" then return command_error("malformed_command", "Pi since must be a non-empty entry id") end
            fields.since = since
        end
    end
    return make_command("get_entries", fields)
end
function M.set_model(provider, model_id, options)
    if type(provider) ~= "string" or provider == "" then return command_error("malformed_command", "Pi provider must be a non-empty string") end
    if type(model_id) ~= "string" or model_id == "" then return command_error("malformed_command", "Pi modelId must be a non-empty string") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.provider = provider
    fields.modelId = model_id
    return make_command("set_model", fields)
end
function M.set_thinking_level(level, options)
    if type(level) ~= "string" or level == "" then return command_error("malformed_command", "Pi thinking level must be a non-empty string") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.level = level
    return make_command("set_thinking_level", fields)
end
function M.compact(custom_instructions, options)
    local fields = options
    if is_object(custom_instructions) then
        fields = custom_instructions
    else
        local copied, err = options_copy(options)
        if err then return nil, err end
        fields = copied
        if custom_instructions ~= nil then
            if type(custom_instructions) ~= "string" then return command_error("malformed_command", "Pi customInstructions must be a string") end
            fields.customInstructions = custom_instructions
        end
    end
    return make_command("compact", fields)
end
function M.get_session_stats(options) return make_command("get_session_stats", options) end
function M.set_auto_retry(enabled, options)
    if type(enabled) ~= "boolean" then return command_error("malformed_command", "Pi auto-retry enabled must be boolean") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.enabled = enabled
    return make_command("set_auto_retry", fields)
end

function M.get_fork_messages(options) return make_command("get_fork_messages", options) end
function M.get_tree(options) return make_command("get_tree", options) end
function M.get_messages(options) return make_command("get_messages", options) end
function M.set_session_name(name, options)
    if type(name) ~= "string" then return command_error("malformed_command", "Pi session name must be a string") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.name = name
    return make_command("set_session_name", fields)
end
function M.cycle_model(options) return make_command("cycle_model", options) end
function M.get_available_models(options) return make_command("get_available_models", options) end
function M.cycle_thinking_level(options) return make_command("cycle_thinking_level", options) end
function M.get_available_thinking_levels(options) return make_command("get_available_thinking_levels", options) end
function M.set_steering_mode(mode, options)
    if mode ~= "all" and mode ~= "one-at-a-time" then return command_error("malformed_command", "Pi steering mode is invalid") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.mode = mode
    return make_command("set_steering_mode", fields)
end
function M.set_follow_up_mode(mode, options)
    if mode ~= "all" and mode ~= "one-at-a-time" then return command_error("malformed_command", "Pi follow-up mode is invalid") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.mode = mode
    return make_command("set_follow_up_mode", fields)
end
function M.set_auto_compaction(enabled, options)
    if type(enabled) ~= "boolean" then return command_error("malformed_command", "Pi auto-compaction enabled must be boolean") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.enabled = enabled
    return make_command("set_auto_compaction", fields)
end
function M.abort_retry(options) return make_command("abort_retry", options) end
function M.bash(command, options)
    if type(command) ~= "string" or command == "" then return command_error("malformed_command", "Pi bash command must be a non-empty string") end
    local fields, err = options_copy(options)
    if err then return nil, err end
    fields.command = command
    return make_command("bash", fields)
end
function M.abort_bash(options) return make_command("abort_bash", options) end
function M.export_html(options) return make_command("export_html", options) end

M.rpc = {
    command = make_command, prompt = M.prompt, steer = M.steer, follow_up = M.follow_up,
    abort = M.abort, get_state = M.get_state, new_session = M.new_session,
    switch_session = M.switch_session, fork = M.fork, clone = M.clone,
    get_fork_messages = M.get_fork_messages, get_entries = M.get_entries,
    get_tree = M.get_tree, get_messages = M.get_messages,
    set_session_name = M.set_session_name, set_model = M.set_model,
    cycle_model = M.cycle_model, get_available_models = M.get_available_models,
    set_thinking_level = M.set_thinking_level, cycle_thinking_level = M.cycle_thinking_level,
    get_available_thinking_levels = M.get_available_thinking_levels,
    set_steering_mode = M.set_steering_mode, set_follow_up_mode = M.set_follow_up_mode,
    compact = M.compact, set_auto_compaction = M.set_auto_compaction,
    get_session_stats = M.get_session_stats, set_auto_retry = M.set_auto_retry,
    abort_retry = M.abort_retry, bash = M.bash, abort_bash = M.abort_bash,
    export_html = M.export_html,
}

local function unsupported(method, reason)
    return { supported = false, method = method, diagnostic = diagnostic("unsupported_control", reason, { method = method }) }
end
function M.start(options)
    if options ~= nil and not is_object(options) then return unsupported("start", "Pi start options must be an object") end
    local result = { supported = true, executable = "pi", arguments = { "--mode", "rpc" }, transport = "stdio_jsonl", framing = "lf_jsonl" }
    if options ~= nil then result.options = copy(options) end
    return result
end
function M.send_text(message, options) return M.prompt(message, options) end
function M.send_keys(_, _) return unsupported("send_keys", "Pi has no RPC keyboard-control command") end
function M.focus(_) return unsupported("focus", "Pi focus is owned by the host terminal, not Pi RPC") end
function M.wait(_) return unsupported("wait", "Pi exposes agent_settled events; it has no RPC wait command") end
function M.pause(_) return unsupported("pause", "Pi exposes abort, not a pause/resume RPC pair") end
function M.resume(_) return unsupported("resume", "Pi exposes prompt/steer/follow_up, not resume") end
function M.retry(options)
    if is_object(options) and options.enabled ~= nil then return M.set_auto_retry(options.enabled, options) end
    return unsupported("retry", "Pi retry is event-driven; use set_auto_retry or observe retry events")
end
function M.install(_) return unsupported("install", "Pi extension installation requires the host transactional installer") end
function M.uninstall(_) return unsupported("uninstall", "Pi extension removal requires the host transactional installer") end
function M.setup(_) return unsupported("setup", "Pi extension setup requires the host transactional installer") end
function M.doctor(_)
    return { supported = true, provider = M.provider, executable = "pi", protocol = copy(M.protocol), diagnostics = {} }
end

return M
