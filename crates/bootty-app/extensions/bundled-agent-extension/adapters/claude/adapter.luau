-- Claude Code's native ingress is authoritative hook JSON or managed stream-json.
-- This module deliberately does not inspect transcripts, terminal output, or process names.

local M = {}

local HOOK_KINDS = {
    SessionStart = "session.start",
    SessionEnd = "session.end",
    Setup = "configuration.setup",
    UserPromptSubmit = "prompt.submit",
    UserPromptExpansion = "prompt.expansion",
    PreToolUse = "tool.pre",
    PostToolUse = "tool.post",
    PostToolUseFailure = "tool.failure",
    PostToolBatch = "tool.batch",
    PermissionRequest = "permission.request",
    PermissionDenied = "permission.denied",
    Notification = "notification",
    MessageDisplay = "notification.message_display",
    SubagentStart = "subagent.start",
    SubagentStop = "subagent.stop",
    Stop = "stop",
    StopFailure = "stop.failure",
    PreCompact = "compaction.pre",
    PostCompact = "compaction.post",
    TaskCreated = "task.created",
    TaskCompleted = "task.completed",
    TeammateIdle = "task.idle",
    WorktreeCreate = "worktree.create",
    WorktreeRemove = "worktree.remove",
    ConfigChange = "configuration.changed",
    InstructionsLoaded = "configuration.instructions_loaded",
    CwdChanged = "configuration.cwd_changed",
    DirectoryAdded = "configuration.directory_added",
    FileChanged = "configuration.file_changed",
    Elicitation = "permission.elicitation",
    ElicitationResult = "permission.elicitation_result",
}

local MANAGED_KINDS = {
    assistant = "stream.assistant",
    user = "stream.user",
    result = "stream.result",
    system = "session.init",
    stream_event = "stream.partial",
    compact_boundary = "stream.compact_boundary",
    status = "stream.status",
    local_command = "stream.local_command",
    hook_started = "stream.hook_started",
    hook_progress = "stream.hook_progress",
    hook_response = "stream.hook_response",
    plugin_install = "stream.plugin_install",
    tool_progress = "stream.tool_progress",
    task_started = "stream.task_started",
    task_progress = "stream.task_progress",
    task_notification = "stream.task_notification",
    notification = "stream.notification",
    rate_limit_event = "stream.rate_limit",
    permission_request = "stream.permission_request",
    permission_response = "stream.permission_response",
    elicitation_request = "stream.elicitation_request",
    elicitation_response = "stream.elicitation_response",
    api_retry = "stream.api_retry",
    auth_status = "stream.auth_status",
    files_persisted = "stream.files_persisted",
}

-- This is the documented SDKControlRequest subtype set. An unknown subtype is
-- not treated as a future event: callers receive an explicit unavailable result.
local CONTROL_SUBTYPES = {
    initialize = true,
    interrupt = true,
    can_use_tool = true,
    request_user_dialog = true,
    set_permission_mode = true,
    set_model = true,
    set_max_thinking_tokens = true,
    apply_flag_settings = true,
    rename_session = true,
    mcp_status = true,
    mcp_reconnect = true,
    mcp_toggle = true,
    mcp_set_servers = true,
    get_context_usage = true,
    get_session_cost = true,
    get_usage = true,
    list_models = true,
    get_binary_version = true,
    mcp_call = true,
    mcp_message = true,
    file_suggestions = true,
    read_file = true,
    rewind_files = true,
    seed_read_state = true,
    register_repo_root = true,
    reload_plugins = true,
    reload_skills = true,
    stop_task = true,
    background_tasks = true,
}

local COMMON_HOOK_FACTS = {
    "session_id",
    "transcript_path",
    "cwd",
    "prompt_id",
    "permission_mode",
    "effort",
    "agent_id",
    "agent_type",
    "hook_id",
    "decision",
    "reason",
    "continue",
    "stopReason",
    "suppressOutput",
    "systemMessage",
    "terminalSequence",
    "hookSpecificOutput",
}

local HOOK_FACTS = {
    SessionStart = { "source", "model", "session_title", "initialUserMessage", "watchPaths", "reloadSkills" },
    SessionEnd = { "reason" },
    Setup = { "trigger" },
    UserPromptSubmit = { "prompt", "session_title", "suppressOriginalPrompt", "additionalContext" },
    UserPromptExpansion = { "expansion_type", "command_name", "command_args", "command_source", "prompt", "additionalContext" },
    PreToolUse = { "tool_name", "tool_input", "tool_use_id" },
    PostToolUse = { "tool_name", "tool_input", "tool_response", "tool_use_id", "duration_ms", "updatedToolOutput", "additionalContext" },
    PostToolUseFailure = { "tool_name", "tool_input", "tool_use_id", "error", "is_interrupt", "duration_ms" },
    PostToolBatch = { "tool_calls", "additionalContext" },
    PermissionRequest = { "tool_name", "tool_input", "permission_suggestions" },
    PermissionDenied = { "tool_name", "tool_input", "tool_use_id", "reason" },
    Notification = { "message", "title", "notification_type" },
    MessageDisplay = { "turn_id", "message_id", "index", "final", "delta", "displayContent" },
    SubagentStart = { "agent_id", "agent_type", "additionalContext" },
    SubagentStop = { "agent_id", "agent_type", "agent_transcript_path", "last_assistant_message", "stop_hook_active", "background_tasks", "session_crons", "additionalContext" },
    Stop = { "stop_hook_active", "last_assistant_message", "background_tasks", "session_crons", "additionalContext" },
    StopFailure = { "error", "error_details", "last_assistant_message" },
    PreCompact = { "trigger", "custom_instructions" },
    PostCompact = { "trigger", "compact_summary" },
    TaskCreated = { "task_id", "task_subject", "task_description", "teammate_name", "team_name" },
    TaskCompleted = { "task_id", "task_subject", "task_description", "teammate_name", "team_name" },
    TeammateIdle = { "teammate_name", "team_name" },
    WorktreeCreate = { "name" },
    WorktreeRemove = { "worktree_path" },
    ConfigChange = { "source", "file_path" },
    InstructionsLoaded = { "file_path", "memory_type", "load_reason", "globs", "trigger_file_path", "parent_file_path" },
    CwdChanged = { "old_cwd", "new_cwd" },
    DirectoryAdded = { "directory", "source" },
    FileChanged = { "file_path", "event" },
    Elicitation = { "mcp_server_name", "message", "mode", "url", "elicitation_id", "requested_schema" },
    ElicitationResult = { "mcp_server_name", "action", "mode", "elicitation_id", "content" },
}

local REQUIRED_HOOK_FIELDS = {
    SessionStart = { { "source", "string" } },
    SessionEnd = { { "reason", "string" } },
    Setup = { { "trigger", "string" } },
    UserPromptSubmit = { { "prompt", "string" } },
    UserPromptExpansion = {
        { "expansion_type", "string" },
        { "command_name", "string" },
        { "command_args", "string" },
        { "prompt", "string" },
    },
    PreToolUse = {
        { "tool_name", "string" },
        { "tool_input", "table" },
        { "tool_use_id", "string" },
    },
    PostToolUse = {
        { "tool_name", "string" },
        { "tool_input", "table" },
        { "tool_response", "any" },
        { "tool_use_id", "string" },
    },
    PostToolUseFailure = {
        { "tool_name", "string" },
        { "tool_input", "table" },
        { "tool_use_id", "string" },
        { "error", "string" },
    },
    PostToolBatch = { { "tool_calls", "table" } },
    PermissionRequest = {
        { "tool_name", "string" },
        { "tool_input", "table" },
    },
    PermissionDenied = {
        { "tool_name", "string" },
        { "tool_input", "table" },
        { "tool_use_id", "string" },
        { "reason", "string" },
    },
    Notification = { { "message", "string" } },
    MessageDisplay = {
        { "turn_id", "string" },
        { "message_id", "string" },
        { "index", "number" },
        { "final", "boolean" },
        { "delta", "string" },
    },
    SubagentStart = {
        { "agent_id", "string" },
        { "agent_type", "string" },
    },
    SubagentStop = {
        { "stop_hook_active", "boolean" },
        { "agent_id", "string" },
        { "agent_type", "string" },
        { "last_assistant_message", "string" },
    },
    Stop = {
        { "stop_hook_active", "boolean" },
        { "last_assistant_message", "string" },
    },
    StopFailure = { { "error", "string" } },
    PreCompact = {
        { "trigger", "string" },
        { "custom_instructions", "string" },
    },
    PostCompact = {
        { "trigger", "string" },
        { "compact_summary", "string" },
    },
    TaskCreated = {
        { "task_id", "string" },
        { "task_subject", "string" },
    },
    TaskCompleted = {
        { "task_id", "string" },
        { "task_subject", "string" },
    },
    TeammateIdle = { { "teammate_name", "string" } },
    WorktreeCreate = { { "name", "string" } },
    WorktreeRemove = { { "worktree_path", "string" } },
    ConfigChange = { { "source", "string" } },
    InstructionsLoaded = {
        { "file_path", "string" },
        { "memory_type", "string" },
        { "load_reason", "string" },
    },
    CwdChanged = {
        { "old_cwd", "string" },
        { "new_cwd", "string" },
    },
    DirectoryAdded = {
        { "directory", "string" },
        { "source", "string" },
    },
    FileChanged = {
        { "file_path", "string" },
        { "event", "string" },
    },
    Elicitation = {
        { "mcp_server_name", "string" },
        { "message", "string" },
    },
    ElicitationResult = {
        { "mcp_server_name", "string" },
        { "action", "string" },
    },
}

local MANAGED_FACTS = {
    "type",
    "subtype",
    "uuid",
    "session_id",
    "parent_tool_use_id",
    "timestamp",
    "ttft_ms",
    "claude_code_version",
    "cwd",
    "tools",
    "mcp_servers",
    "model",
    "permissionMode",
    "slash_commands",
    "output_style",
    "skills",
    "plugins",
    "capabilities",
    "message",
    "event",
    "duration_ms",
    "duration_api_ms",
    "is_error",
    "num_turns",
    "result",
    "stop_reason",
    "user_message_uuid",
    "total_cost_usd",
    "usage",
    "modelUsage",
    "permission_denials",
    "deferred_tool_use",
    "terminal_reason",
    "errors",
    "request_id",
    "request",
    "response",
}

local function diagnostic(code, message, field, detail)
    local item = {
        adapter = "claude",
        authoritative = true,
        code = code,
        kind = code,
        message = message,
    }
    if field ~= nil then
        item.field = field
    end
    if detail ~= nil then
        item.detail = detail
    end
    return item
end

local function failure(code, message, field, detail)
    local item = diagnostic(code, message, field, detail)
    return {
        ok = false,
        events = {},
        diagnostics = { item },
        error = item,
    }
end

local function success(event)
    return {
        ok = true,
        events = { event },
        diagnostics = {},
    }
end

local function copy_fields(source, target, fields)
    local index
    for index = 1, #fields do
        local field = fields[index]
        if source[field] ~= nil then
            target[field] = source[field]
        end
    end
end

local function check_field(payload, field, expected)
    local value = payload[field]
    if value == nil then
        return false, "missing required field '" .. field .. "'"
    end
    if expected ~= "any" and type(value) ~= expected then
        return false, "field '" .. field .. "' must be " .. expected
    end
    return true, nil
end

local function first_string(payload, fields)
    local index
    for index = 1, #fields do
        local value = payload[fields[index]]
        if type(value) == "string" then
            return value
        end
    end
    return nil
end

local function hook_native_id(payload, event_name)
    local event_id = first_string(payload, {
        "hook_id",
        "prompt_id",
        "tool_use_id",
        "agent_id",
        "task_id",
        "elicitation_id",
        "message_id",
        "turn_id",
        "worktree_path",
    })
    if event_id ~= nil then
        return event_id
    end
    -- A session id is a native opaque id, but it is not synthesized into an
    -- event sequence. Events without any native id leave native_id absent.
    if event_name == "SessionStart" or event_name == "SessionEnd" then
        return first_string(payload, { "session_id" })
    end
    return nil
end

local function make_event(kind, native_id, session_id, facts, raw)
    local event = {
        adapter = "claude",
        kind = kind,
        facts = facts,
        raw = raw,
    }
    if native_id ~= nil then
        event.native_id = native_id
    end
    if session_id ~= nil then
        event.adapter_session_id = session_id
    end
    return event
end

local function validate_post_batch(payload)
    local calls = payload.tool_calls
    local index
    for index = 1, #calls do
        local call = calls[index]
        if type(call) ~= "table" then
            return false, "tool_calls[" .. index .. "] must be an object", "tool_calls"
        end
        local fields = {
            { "tool_name", "string" },
            { "tool_input", "table" },
            { "tool_use_id", "string" },
        }
        local field_index
        for field_index = 1, #fields do
            local valid, message = check_field(call, fields[field_index][1], fields[field_index][2])
            if not valid then
                return false, "tool_calls[" .. index .. "]: " .. message, "tool_calls"
            end
        end
        if call.tool_response == nil then
            return false, "tool_calls[" .. index .. "] is missing required field 'tool_response'", "tool_calls"
        end
    end
    return true, nil, nil
end

local function normalize_hook(payload)
    if type(payload) ~= "table" then
        return failure("invalid_payload", "hook payload must be a decoded JSON object", nil, "expected table")
    end
    local event_name = payload.hook_event_name
    if type(event_name) ~= "string" then
        return failure("malformed_hook_event", "hook payload is missing string 'hook_event_name'", "hook_event_name")
    end
    local kind = HOOK_KINDS[event_name]
    if kind == nil then
        return failure("unknown_hook_event", "unsupported authoritative Claude hook event '" .. event_name .. "'", "hook_event_name", event_name)
    end

    local required = REQUIRED_HOOK_FIELDS[event_name]
    local index
    if required ~= nil then
        for index = 1, #required do
            local field = required[index][1]
            local expected = required[index][2]
            local valid, message = check_field(payload, field, expected)
            if not valid then
                return failure("malformed_hook_event", message, field, event_name)
            end
        end
    end
    if event_name == "PostToolBatch" then
        local valid, message, field = validate_post_batch(payload)
        if not valid then
            return failure("malformed_hook_event", message, field, event_name)
        end
    end

    local facts = { native_type = event_name }
    copy_fields(payload, facts, COMMON_HOOK_FACTS)
    local event_fields = HOOK_FACTS[event_name]
    if event_fields ~= nil then
        copy_fields(payload, facts, event_fields)
    end

    local native_id = hook_native_id(payload, event_name)
    local session_id = payload.session_id
    if session_id ~= nil and type(session_id) ~= "string" then
        return failure("malformed_hook_event", "field 'session_id' must be string when present", "session_id", event_name)
    end
    return success(make_event(kind, native_id, session_id, facts, payload))
end

local function require_managed_field(payload, field, expected, message)
    local valid, detail = check_field(payload, field, expected)
    if not valid then
        return failure("malformed_stream_message", message or detail, field, payload.type)
    end
    return nil
end

local function validate_control_request(payload)
    local failure_result = require_managed_field(payload, "request_id", "string")
    if failure_result ~= nil then
        return failure_result
    end
    failure_result = require_managed_field(payload, "request", "table")
    if failure_result ~= nil then
        return failure_result
    end
    local subtype = payload.request.subtype
    if type(subtype) ~= "string" then
        return failure("malformed_control_request", "control request is missing string 'request.subtype'", "request.subtype")
    end
    if not CONTROL_SUBTYPES[subtype] then
        return failure("unknown_control_subtype", "unsupported authoritative Claude control subtype '" .. subtype .. "'", "request.subtype", subtype)
    end
    return nil
end

local function validate_control_response(payload)
    if type(payload.response) ~= "table" then
        return failure("malformed_control_response", "control response is missing object 'response'", "response")
    end
    local response = payload.response
    if type(response.subtype) ~= "string" then
        return failure("malformed_control_response", "control response is missing string 'response.subtype'", "response.subtype")
    end
    if response.subtype ~= "success" and response.subtype ~= "error" then
        return failure("unknown_control_subtype", "unsupported authoritative control response subtype '" .. response.subtype .. "'", "response.subtype", response.subtype)
    end
    if type(response.request_id) ~= "string" then
        return failure("malformed_control_response", "control response is missing string 'response.request_id'", "response.request_id")
    end
    if response.subtype == "error" and type(response.error) ~= "string" then
        return failure("malformed_control_response", "error control response is missing string 'response.error'", "response.error")
    end
    return nil
end

local function validate_managed(payload)
    if type(payload) ~= "table" then
        return failure("invalid_payload", "managed payload must be a decoded JSON object", nil, "expected table")
    end
    local message_type = payload.type
    if type(message_type) ~= "string" then
        return failure("malformed_stream_message", "managed payload is missing string 'type'", "type")
    end
    if message_type == "control_request" then
        local failure_result = validate_control_request(payload)
        if failure_result ~= nil then
            return failure_result
        end
    elseif message_type == "control_response" then
        local failure_result = validate_control_response(payload)
        if failure_result ~= nil then
            return failure_result
        end
    elseif MANAGED_KINDS[message_type] == nil then
        return failure("unknown_stream_message", "unsupported authoritative Claude stream message '" .. message_type .. "'", "type", message_type)
    elseif message_type == "assistant" then
        local failure_result = require_managed_field(payload, "uuid", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "session_id", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "message", "table")
        if failure_result ~= nil then
            return failure_result
        end
    elseif message_type == "user" then
        local failure_result = require_managed_field(payload, "message", "table")
        if failure_result ~= nil then
            return failure_result
        end
        if payload.uuid ~= nil and type(payload.uuid) ~= "string" then
            return failure("malformed_stream_message", "field 'uuid' must be string when present", "uuid", message_type)
        end
    elseif message_type == "stream_event" then
        local failure_result = require_managed_field(payload, "uuid", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "session_id", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "event", "table")
        if failure_result ~= nil then
            return failure_result
        end
    elseif message_type == "system" then
        local failure_result = require_managed_field(payload, "uuid", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "session_id", "string")
        if failure_result ~= nil then
            return failure_result
        end
        if payload.subtype ~= "init" then
            return failure("unknown_stream_subtype", "unsupported authoritative system subtype", "subtype", payload.subtype)
        end
    elseif message_type == "result" then
        local failure_result = require_managed_field(payload, "uuid", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "session_id", "string")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "subtype", "string")
        if failure_result ~= nil then
            return failure_result
        end
        if payload.subtype ~= "success"
            and payload.subtype ~= "error_max_turns"
            and payload.subtype ~= "error_during_execution"
            and payload.subtype ~= "error_max_budget_usd"
            and payload.subtype ~= "error_max_structured_output_retries" then
            return failure("unknown_stream_subtype", "unsupported authoritative result subtype '" .. tostring(payload.subtype) .. "'", "subtype", payload.subtype)
        end
        failure_result = require_managed_field(payload, "is_error", "boolean")
        if failure_result ~= nil then
            return failure_result
        end
        failure_result = require_managed_field(payload, "num_turns", "number")
        if failure_result ~= nil then
            return failure_result
        end
        if payload.subtype == "success" then
            failure_result = require_managed_field(payload, "result", "string")
            if failure_result ~= nil then
                return failure_result
            end
        else
            failure_result = require_managed_field(payload, "errors", "table")
            if failure_result ~= nil then
                return failure_result
            end
        end
    else
        -- All other SDK messages carry an opaque UUID. We do not interpret their
        -- rendered text and only validate the native identity envelope.
        local failure_result = require_managed_field(payload, "uuid", "string")
        if failure_result ~= nil then
            return failure_result
        end
    end
    return nil
end

local function managed_native_id(payload)
    if type(payload.uuid) == "string" then
        return payload.uuid
    end
    if payload.type == "control_request" and type(payload.request_id) == "string" then
        return payload.request_id
    end
    if payload.type == "control_response" and type(payload.response) == "table" and type(payload.response.request_id) == "string" then
        return payload.response.request_id
    end
    return nil
end

local function normalize_managed(payload)
    local failure_result = validate_managed(payload)
    if failure_result ~= nil then
        return failure_result
    end
    local message_type = payload.type
    local kind
    if message_type == "control_request" then
        kind = "control.request"
    elseif message_type == "control_response" then
        kind = "control.response"
    else
        kind = MANAGED_KINDS[message_type]
    end

    local facts = { native_type = message_type }
    copy_fields(payload, facts, MANAGED_FACTS)
    local session_id = payload.session_id
    return success(make_event(kind, managed_native_id(payload), session_id, facts, payload))
end

local function normalized_mode(mode)
    if mode == nil then
        return "interactive"
    end
    if mode == "interactive" or mode == "hooks" or mode == "hook" then
        return "interactive"
    end
    if mode == "managed" or mode == "stream" or mode == "stream-json" then
        return "managed"
    end
    return nil
end

function M.normalize(raw, mode)
    local selected = normalized_mode(mode)
    if selected == nil then
        return failure("invalid_mode", "Claude adapter mode must be interactive/hooks or managed/stream-json", "mode", mode)
    end
    if type(raw) == "string" then
        return failure("invalid_payload", "Claude adapter expects a decoded JSON object; it never parses terminal or transcript text", nil, "string input is not authoritative")
    end
    if selected == "managed" then
        return normalize_managed(raw)
    end
    return normalize_hook(raw)
end

-- The generic extension forwarder uses the same pure normalization boundary.
M.forward = M.normalize

function M.start(options)
    if options ~= nil and type(options) ~= "table" then
        return { supported = false, diagnostic = diagnostic("malformed_command", "Claude start options must be an object") }
    end
    return {
        supported = true,
        executable = "claude",
        arguments = {
            "-p", "--input-format", "stream-json", "--output-format", "stream-json",
            "--verbose", "--include-partial-messages", "--include-hook-events",
            "--forward-subagent-text", "--replay-user-messages",
        },
        transport = "stdio_jsonl",
        framing = "lf_jsonl",
    }
end

local function control_request(subtype, options, record)
    options = options or {}
    local request_id = options.request_id or options.native_id or (record and record.native_id)
    if type(request_id) ~= "string" and type(request_id) ~= "number" then
        return { supported = false, diagnostic = diagnostic("missing_request_id", "Claude control requires an opaque request_id") }
    end
    return {
        type = "control_request",
        request_id = request_id,
        request = { subtype = subtype },
    }
end

function M.prompt(message, options)
    if type(message) ~= "string" then
        return { supported = false, diagnostic = diagnostic("malformed_command", "Claude prompt must be a string") }
    end
    return {
        type = "user",
        message = { role = "user", content = message },
        session_id = options and options.adapter_session_id or nil,
    }
end
M.send_text = M.prompt
M.abort = function(options, record) return control_request("interrupt", options, record) end
M.pause = function() return { supported = false, diagnostic = diagnostic("unsupported_control", "Claude pause is unavailable") } end
M.resume = function() return { supported = false, diagnostic = diagnostic("unsupported_control", "Claude resume is unavailable") } end
M.retry = function() return { supported = false, diagnostic = diagnostic("unsupported_control", "Claude retry is unavailable") } end
M.send_keys = function() return { supported = false, diagnostic = diagnostic("unsupported_control", "Claude keyboard control is unavailable") } end

M.manifest = {
    id = "claude",
    provider = "claude",
    entrypoint = "adapter.luau",
    modes = { "interactive", "managed" },
    authority = { "hooks", "stream-json" },
}

return M
