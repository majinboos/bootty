local adapter = {}

local PROVIDER = "codex"
local VERSION = "0.0.0"
local SOURCE_REVISION = "50ef7395faee1d0e2d01730f9636aa0601c7be3"

adapter.provider = PROVIDER
adapter.version = VERSION
adapter.source_revision = SOURCE_REVISION

local function copy(value)
  if type(value) ~= "table" then
    return value
  end
  local result = {}
  for key, item in pairs(value) do
    result[key] = copy(item)
  end
  return result
end

local function is_id(value)
  return type(value) == "string" or type(value) == "number"
end

local function is_string(value)
  return type(value) == "string" and value ~= ""
end

local function add_if_present(target, key, value)
  if value ~= nil then
    target[key] = copy(value)
  end
end

local function diagnostic(code, message, raw, details)
  local body = {
    ok = false,
    adapter = PROVIDER,
    kind = "diagnostic",
    code = code,
    message = message,
    diagnostic = {
      code = code,
      message = message,
    },
  }
  if details ~= nil then
    body.facts = copy(details)
    for key, value in pairs(details) do
      body[key] = copy(value)
      body.diagnostic[key] = copy(value)
    end
  end
  if raw ~= nil then
    body.raw = copy(raw)
  end
  return body
end

adapter.diagnostic = diagnostic

-- A bounded JSON decoder keeps fixture execution independent of the Bootty host.
-- The host decoder is preferred, but this fallback only accepts JSON objects.
local function fallback_json(text)
  local index = 1
  local length = #text

  local function skip_space()
    while index <= length do
      local character = text:sub(index, index)
      if character == " " or character == "\t" or character == "\r" or character == "\n" then
        index = index + 1
      else
        return
      end
    end
  end

  local parse_value

  local function parse_string()
    if text:sub(index, index) ~= "\"" then
      error("expected JSON string")
    end
    index = index + 1
    local result = {}
    while index <= length do
      local character = text:sub(index, index)
      index = index + 1
      if character == "\"" then
        return table.concat(result)
      end
      if character == "\\" then
        local escaped = text:sub(index, index)
        index = index + 1
        local replacements = {
          ["\""] = "\"",
          ["\\"] = "\\",
          ["/"] = "/",
          ["b"] = "\b",
          ["f"] = "\f",
          ["n"] = "\n",
          ["r"] = "\r",
          ["t"] = "\t",
        }
        if replacements[escaped] ~= nil then
          result[#result + 1] = replacements[escaped]
        elseif escaped == "u" then
          local digits = text:sub(index, index + 3)
          if not digits:match("^%x%x%x%x$") then
            error("invalid JSON unicode escape")
          end
          local codepoint = tonumber(digits, 16)
          if utf8 ~= nil and utf8.char ~= nil then
            result[#result + 1] = utf8.char(codepoint)
          else
            result[#result + 1] = "?"
          end
          index = index + 4
        else
          error("invalid JSON escape")
        end
      elseif string.byte(character) < 0x20 then
        error("JSON string contains a control character")
      else
        result[#result + 1] = character
      end
    end
    error("unterminated JSON string")
  end

  local function parse_array()
    index = index + 1
    local result = {}
    skip_space()
    if text:sub(index, index) == "]" then
      index = index + 1
      return result
    end
    while true do
      result[#result + 1] = parse_value()
      skip_space()
      local character = text:sub(index, index)
      if character == "]" then
        index = index + 1
        return result
      end
      if character ~= "," then
        error("expected JSON array separator")
      end
      index = index + 1
      skip_space()
    end
  end

  local function parse_object()
    index = index + 1
    local result = {}
    skip_space()
    if text:sub(index, index) == "}" then
      index = index + 1
      return result
    end
    while true do
      skip_space()
      local key = parse_string()
      skip_space()
      if text:sub(index, index) ~= ":" then
        error("expected JSON object colon")
      end
      index = index + 1
      skip_space()
      result[key] = parse_value()
      skip_space()
      local character = text:sub(index, index)
      if character == "}" then
        index = index + 1
        return result
      end
      if character ~= "," then
        error("expected JSON object separator")
      end
      index = index + 1
    end
  end

  local function parse_number()
    local start = index
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
      error("invalid JSON number")
    end
    if text:sub(index, index) == "." then
      index = index + 1
      if not text:sub(index, index):match("%d") then
        error("invalid JSON fraction")
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
        error("invalid JSON exponent")
      end
      while text:sub(index, index):match("%d") do
        index = index + 1
      end
    end
    local number = tonumber(text:sub(start, index - 1))
    if number == nil then
      error("invalid JSON number")
    end
    return number
  end

  function parse_value()
    skip_space()
    local character = text:sub(index, index)
    if character == "\"" then
      return parse_string()
    end
    if character == "{" then
      return parse_object()
    end
    if character == "[" then
      return parse_array()
    end
    if character == "-" or character:match("%d") then
      return parse_number()
    end
    if text:sub(index, index + 3) == "true" then
      index = index + 4
      return true
    end
    if text:sub(index, index + 4) == "false" then
      index = index + 5
      return false
    end
    if text:sub(index, index + 3) == "null" then
      index = index + 4
      return nil
    end
    error("invalid JSON value")
  end

  local result = parse_value()
  skip_space()
  if index <= length then
    error("trailing JSON data")
  end
  if type(result) ~= "table" then
    error("JSON root must be an object")
  end
  return result
end

local function decode(value)
  if type(value) == "table" then
    return value
  end
  if type(value) ~= "string" then
    return nil, diagnostic("malformed_event", "event must be a table or JSON string", value)
  end
  if type(bootty) == "table" and type(bootty.json) == "table" and type(bootty.json.decode) == "function" then
    local ok, result = pcall(bootty.json.decode, value)
    if ok and type(result) == "table" then
      return result
    end
  end
  local ok, result = pcall(fallback_json, value)
  if not ok or type(result) ~= "table" then
    return nil, diagnostic("malformed_json", "event JSON could not be decoded as an object", value)
  end
  return result
end

adapter.decode = decode

local function check_context(context, raw)
  if context == nil then
    context = {}
  end
  if type(context) ~= "table" then
    return nil, diagnostic("malformed_context", "forwarding context must be a table", raw)
  end
  local metadata = {}
  local sequence = context.sequence
  if sequence ~= nil then
    if type(sequence) ~= "number" then
      return nil, diagnostic("malformed_context", "sequence must be numeric when supplied", raw, {
        field = "sequence",
      })
    end
    metadata.sequence = sequence
  end
  local terminal_id = context.terminal_id
  if terminal_id ~= nil then
    if not is_id(terminal_id) then
      return nil, diagnostic("malformed_context", "terminal_id must be an opaque string or number", raw, {
        field = "terminal_id",
      })
    end
    metadata.terminal_id = terminal_id
  end
  local process_id = context.process_id
  if process_id ~= nil then
    if not is_id(process_id) then
      return nil, diagnostic("malformed_context", "process_id must be an opaque string or number", raw, {
        field = "process_id",
      })
    end
    metadata.process_id = process_id
  end
  local adapter_session_id = context.adapter_session_id
  if adapter_session_id ~= nil then
    if not is_id(adapter_session_id) then
      return nil, diagnostic("malformed_context", "adapter_session_id must be an opaque string or number", raw, {
        field = "adapter_session_id",
      })
    end
    metadata.adapter_session_id = adapter_session_id
  end
  local thread_id = context.thread_id
  if thread_id ~= nil then
    if not is_id(thread_id) then
      return nil, diagnostic("malformed_context", "thread_id must be an opaque string or number", raw, {
        field = "thread_id",
      })
    end
    metadata.thread_id = thread_id
  end
  return metadata
end

local function envelope(kind, native_id, adapter_session_id, facts, raw, metadata)
  local event = {
    adapter = PROVIDER,
    kind = kind,
    native_id = native_id,
    facts = facts or {},
    raw = copy(raw),
  }
  if adapter_session_id ~= nil then
    event.adapter_session_id = adapter_session_id
  end
  if metadata ~= nil then
    add_if_present(event, "sequence", metadata.sequence)
    add_if_present(event, "terminal_id", metadata.terminal_id)
    add_if_present(event, "process_id", metadata.process_id)
  end
  return event
end

local hook_kinds = {
  SessionStart = "hook_session_started",
  SessionEnd = "hook_session_ended",
  UserPromptSubmit = "hook_prompt_submitted",
  PreToolUse = "hook_tool_before",
  PermissionRequest = "hook_permission_requested",
  PostToolUse = "hook_tool_after",
  PreCompact = "hook_compaction_before",
  PostCompact = "hook_compaction_after",
  SubagentStart = "hook_subagent_started",
  SubagentStop = "hook_subagent_stopped",
  Stop = "hook_stop",
}

local turn_hooks = {
  UserPromptSubmit = true,
  PreToolUse = true,
  PermissionRequest = true,
  PostToolUse = true,
  PreCompact = true,
  PostCompact = true,
  SubagentStart = true,
  SubagentStop = true,
  Stop = true,
}

local function require_field(input, field, kind, predicate, description)
  local value = input[field]
  if value == nil then
    return nil, diagnostic("malformed_event", "missing required hook field " .. field, input, {
      event = kind,
      field = field,
    })
  end
  if predicate ~= nil and not predicate(value) then
    return nil, diagnostic("malformed_event", "invalid hook field " .. field, input, {
      event = kind,
      field = field,
      expected = description,
    })
  end
  return value
end

local function allowed(value, values)
  for _, candidate in ipairs(values) do
    if value == candidate then
      return true
    end
  end
  return false
end

local function hook_facts(input)
  local facts = {}
  local fields = {
    "hook_event_name",
    "session_id",
    "turn_id",
    "agent_id",
    "agent_type",
    "transcript_path",
    "agent_transcript_path",
    "cwd",
    "model",
    "permission_mode",
    "source",
    "reason",
    "trigger",
    "tool_name",
    "tool_input",
    "tool_response",
    "tool_use_id",
    "prompt",
    "stop_hook_active",
    "last_assistant_message",
  }
  for _, field in ipairs(fields) do
    add_if_present(facts, field, input[field])
  end
  return facts
end

local function hook_native_id(name, input)
  if (name == "SubagentStart" or name == "SubagentStop") and input.agent_id ~= nil then
    return input.agent_id
  end
  if (name == "PreToolUse" or name == "PostToolUse") and input.tool_use_id ~= nil then
    return input.tool_use_id
  end
  if (name == "PreCompact" or name == "PostCompact") and input.turn_id ~= nil then
    return input.turn_id
  end
  if name == "Stop" and input.turn_id ~= nil then
    return input.turn_id
  end
  return input.session_id
end

local function parse_hook(value, context)
  local input, decode_error = decode(value)
  if input == nil then
    return decode_error
  end
  local metadata, context_error = check_context(context, input)
  if metadata == nil then
    return context_error
  end
  local name = input.hook_event_name
  if not is_string(name) or hook_kinds[name] == nil then
    return diagnostic("malformed_event", "unknown or missing Codex hook_event_name", input, {
      field = "hook_event_name",
    })
  end
  local session_id, session_error = require_field(input, "session_id", name, is_string, "non-empty string")
  if session_id == nil then
    return session_error
  end
  if turn_hooks[name] then
    local turn_id, turn_error = require_field(input, "turn_id", name, is_string, "non-empty string")
    if turn_id == nil then
      return turn_error
    end
  end
  if name == "SessionStart" then
    local source, source_error = require_field(input, "source", name, is_string, "startup, resume, clear, or compact")
    if source == nil then
      return source_error
    end
    if not allowed(source, { "startup", "resume", "clear", "compact" }) then
      return diagnostic("malformed_event", "unsupported SessionStart source", input, {
        field = "source",
      })
    end
  elseif name == "SessionEnd" then
    local reason, reason_error = require_field(input, "reason", name, is_string, "other")
    if reason == nil then
      return reason_error
    end
    if reason ~= "other" then
      return diagnostic("malformed_event", "unsupported SessionEnd reason", input, {
        field = "reason",
      })
    end
  elseif name == "UserPromptSubmit" then
    local prompt, prompt_error = require_field(input, "prompt", name, function(item)
      return type(item) == "string"
    end, "string")
    if prompt == nil then
      return prompt_error
    end
  elseif name == "PreToolUse" then
    local tool_name, tool_error = require_field(input, "tool_name", name, is_string, "non-empty string")
    if tool_name == nil then
      return tool_error
    end
    local tool_input, input_error = require_field(input, "tool_input", name, function(item)
      return type(item) == "table"
    end, "object")
    if tool_input == nil then
      return input_error
    end
    local tool_use_id, id_error = require_field(input, "tool_use_id", name, is_string, "non-empty string")
    if tool_use_id == nil then
      return id_error
    end
  elseif name == "PermissionRequest" then
    local tool_name, tool_error = require_field(input, "tool_name", name, is_string, "non-empty string")
    if tool_name == nil then
      return tool_error
    end
    local tool_input, input_error = require_field(input, "tool_input", name, function(item)
      return type(item) == "table"
    end, "object")
    if tool_input == nil then
      return input_error
    end
  elseif name == "PostToolUse" then
    local tool_name, tool_error = require_field(input, "tool_name", name, is_string, "non-empty string")
    if tool_name == nil then
      return tool_error
    end
    local tool_input, input_error = require_field(input, "tool_input", name, function(item)
      return type(item) == "table"
    end, "object")
    if tool_input == nil then
      return input_error
    end
    local tool_response, response_error = require_field(input, "tool_response", name, function(item)
      return type(item) == "table"
    end, "object")
    if tool_response == nil then
      return response_error
    end
    local tool_use_id, id_error = require_field(input, "tool_use_id", name, is_string, "non-empty string")
    if tool_use_id == nil then
      return id_error
    end
  elseif name == "PreCompact" or name == "PostCompact" then
    local trigger, trigger_error = require_field(input, "trigger", name, is_string, "manual or auto")
    if trigger == nil then
      return trigger_error
    end
    if not allowed(trigger, { "manual", "auto" }) then
      return diagnostic("malformed_event", "unsupported compaction trigger", input, {
        field = "trigger",
      })
    end
  elseif name == "SubagentStart" then
    local agent_id, id_error = require_field(input, "agent_id", name, is_string, "non-empty string")
    if agent_id == nil then
      return id_error
    end
    local agent_type, type_error = require_field(input, "agent_type", name, is_string, "non-empty string")
    if agent_type == nil then
      return type_error
    end
  elseif name == "SubagentStop" then
    local agent_id, id_error = require_field(input, "agent_id", name, is_string, "non-empty string")
    if agent_id == nil then
      return id_error
    end
    local agent_type, type_error = require_field(input, "agent_type", name, is_string, "non-empty string")
    if agent_type == nil then
      return type_error
    end
    local stop_active, stop_error = require_field(input, "stop_hook_active", name, function(item)
      return type(item) == "boolean"
    end, "boolean")
    if stop_active == nil then
      return stop_error
    end
  elseif name == "Stop" then
    local stop_active, stop_error = require_field(input, "stop_hook_active", name, function(item)
      return type(item) == "boolean"
    end, "boolean")
    if stop_active == nil then
      return stop_error
    end
  end
  local facts = hook_facts(input)
  facts.native_event = name
  local native_id = hook_native_id(name, input)
  local adapter_session_id = metadata.adapter_session_id or session_id
  return envelope(hook_kinds[name], native_id, adapter_session_id, facts, input, metadata)
end

adapter.parse_hook = parse_hook
adapter.forward_hook = parse_hook

local method_kinds = {
  ["thread/started"] = "managed_thread_started",
  ["thread/status/changed"] = "managed_thread_status_changed",
  ["thread/closed"] = "managed_thread_closed",
  ["thread/tokenUsage/updated"] = "managed_token_usage_updated",
  ["turn/started"] = "managed_turn_started",
  ["turn/completed"] = "managed_turn_completed",
  ["turn/plan/updated"] = "managed_plan_updated",
  ["item/started"] = "managed_item_started",
  ["item/completed"] = "managed_item_completed",
  ["item/agentMessage/delta"] = "managed_agent_message_delta",
  ["item/plan/delta"] = "managed_plan_delta",
  ["item/reasoning/summaryTextDelta"] = "managed_reasoning_delta",
  ["item/reasoning/summaryPartAdded"] = "managed_reasoning_part_added",
  ["item/commandExecution/outputDelta"] = "managed_command_output_delta",
  ["item/commandExecution/terminalInteraction"] = "managed_command_terminal_interaction",
  ["item/fileChange/outputDelta"] = "managed_file_change_output_delta",
  ["item/mcpToolCall/progress"] = "managed_mcp_progress",
  ["hook/started"] = "managed_hook_started",
  ["hook/completed"] = "managed_hook_completed",
  ["thread/compacted"] = "managed_compaction_completed",
  ["serverRequest/resolved"] = "managed_approval_resolved",
  ["item/commandExecution/requestApproval"] = "managed_approval_requested",
  ["item/fileChange/requestApproval"] = "managed_approval_requested",
  ["item/permissions/requestApproval"] = "managed_approval_requested",
}

local client_methods = {
  initialize = true,
  initialized = true,
  ["thread/start"] = true,
  ["thread/resume"] = true,
  ["thread/fork"] = true,
  ["thread/unsubscribe"] = true,
  ["thread/archive"] = true,
  ["thread/delete"] = true,
  ["thread/unarchive"] = true,
  ["thread/settings/update"] = true,
  ["thread/inject_items"] = true,
  ["thread/compact/start"] = true,
  ["thread/shellCommand"] = true,
  ["turn/start"] = true,
  ["turn/steer"] = true,
  ["turn/interrupt"] = true,
}

local approval_methods = {
  ["item/commandExecution/requestApproval"] = true,
  ["item/fileChange/requestApproval"] = true,
  ["item/permissions/requestApproval"] = true,
}

local function require_param(params, method, field, predicate, description, raw)
  local value = params[field]
  if value == nil then
    return nil, diagnostic("malformed_event", "missing required app-server field " .. field, raw, {
      method = method,
      field = field,
    })
  end
  if predicate ~= nil and not predicate(value) then
    return nil, diagnostic("malformed_event", "invalid app-server field " .. field, raw, {
      method = method,
      field = field,
      expected = description,
    })
  end
  return value
end

local function string_param(params, method, field, raw)
  return require_param(params, method, field, is_string, "non-empty string", raw)
end

local function validate_initialize(params, raw)
  local client_info, client_error = require_param(params, "initialize", "clientInfo", function(value)
    return type(value) == "table" and is_string(value.name)
  end, "object with clientInfo.name", raw)
  if client_info == nil then
    return client_error
  end
  return true
end

local function validate_client_params(method, params, raw)
  if method == "initialize" then
    return validate_initialize(params, raw)
  end
  if method == "initialized" then
    return true
  end
  if method == "thread/start" then
    return true
  end
  if method == "thread/resume" or method == "thread/fork" then
    local _, error_body = string_param(params, method, "threadId", raw)
    if error_body ~= nil then
      return error_body
    end
    return true
  end
  if method == "thread/compact/start" or method == "thread/shellCommand" then
    local _, error_body = string_param(params, method, "threadId", raw)
    if error_body ~= nil then
      return error_body
    end
    return true
  end
  if method == "turn/start" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, input_error = require_param(params, method, "input", function(value)
      return type(value) == "table"
    end, "array", raw)
    if input_error ~= nil then
      return input_error
    end
    return true
  end
  if method == "turn/steer" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, expected_error = string_param(params, method, "expectedTurnId", raw)
    if expected_error ~= nil then
      return expected_error
    end
    local _, input_error = require_param(params, method, "input", function(value)
      return type(value) == "table"
    end, "array", raw)
    if input_error ~= nil then
      return input_error
    end
    return true
  end
  if method == "turn/interrupt" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, turn_error = string_param(params, method, "turnId", raw)
    if turn_error ~= nil then
      return turn_error
    end
    return true
  end
  return true
end

local function validate_notification_params(method, params, raw)
  if method == "thread/started" then
    local thread, thread_error = require_param(params, method, "thread", function(value)
      return type(value) == "table" and is_string(value.id)
    end, "thread object with id", raw)
    if thread == nil then
      return thread_error
    end
  elseif method == "thread/status/changed" or method == "thread/closed" or method == "thread/tokenUsage/updated" then
    local _, error_body = string_param(params, method, "threadId", raw)
    if error_body ~= nil then
      return error_body
    end
  elseif method == "turn/started" or method == "turn/completed" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local turn, turn_error = require_param(params, method, "turn", function(value)
      return type(value) == "table" and is_string(value.id)
    end, "turn object with id", raw)
    if turn == nil then
      return turn_error
    end
  elseif method == "turn/plan/updated" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, turn_error = string_param(params, method, "turnId", raw)
    if turn_error ~= nil then
      return turn_error
    end
  elseif method == "item/started" or method == "item/completed" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, turn_error = string_param(params, method, "turnId", raw)
    if turn_error ~= nil then
      return turn_error
    end
    local item, item_error = require_param(params, method, "item", function(value)
      return type(value) == "table" and is_string(value.id)
    end, "item object with id", raw)
    if item == nil then
      return item_error
    end
  elseif method == "item/commandExecution/requestApproval" or method == "item/fileChange/requestApproval" or method == "item/permissions/requestApproval" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, turn_error = string_param(params, method, "turnId", raw)
    if turn_error ~= nil then
      return turn_error
    end
    local _, item_error = string_param(params, method, "itemId", raw)
    if item_error ~= nil then
      return item_error
    end
  elseif method == "serverRequest/resolved" then
    local request_id = params.requestId
    if request_id == nil and params.id ~= nil then
      request_id = params.id
    end
    if not is_id(request_id) then
      return diagnostic("malformed_event", "serverRequest/resolved requires requestId", raw, {
        method = method,
        field = "requestId",
      })
    end
  elseif method == "thread/compacted" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, turn_error = string_param(params, method, "turnId", raw)
    if turn_error ~= nil then
      return turn_error
    end
  elseif method == "item/agentMessage/delta" or method == "item/plan/delta" or method == "item/reasoning/summaryTextDelta" or method == "item/reasoning/summaryPartAdded" or method == "item/commandExecution/outputDelta" or method == "item/commandExecution/terminalInteraction" or method == "item/fileChange/outputDelta" or method == "item/mcpToolCall/progress" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
    local _, turn_error = string_param(params, method, "turnId", raw)
    if turn_error ~= nil then
      return turn_error
    end
    local item_id = params.itemId
    if item_id == nil and type(params.item) == "table" then
      item_id = params.item.id
    end
    if not is_string(item_id) then
      return diagnostic("malformed_event", "item delta requires itemId", raw, {
        method = method,
        field = "itemId",
      })
    end
  elseif method == "hook/started" or method == "hook/completed" then
    local _, thread_error = string_param(params, method, "threadId", raw)
    if thread_error ~= nil then
      return thread_error
    end
  end
  return true
end

local function facts_from_params(method, params, message_id)
  local facts = {
    method = method,
  }
  add_if_present(facts, "rpc_id", message_id)
  local direct_fields = {
    { "thread_id", "threadId" },
    { "turn_id", "turnId" },
    { "item_id", "itemId" },
    { "request_id", "requestId" },
    { "approval_id", "approvalId" },
    { "delta", "delta" },
    { "explanation", "explanation" },
    { "plan", "plan" },
    { "status", "status" },
    { "token_usage", "tokenUsage" },
    { "scope", "scope" },
    { "permissions", "permissions" },
    { "decision", "decision" },
  }
  for _, fields in ipairs(direct_fields) do
    add_if_present(facts, fields[1], params[fields[2]])
  end
  if type(params.thread) == "table" then
    add_if_present(facts, "thread", params.thread)
    add_if_present(facts, "thread_id", params.thread.id)
    add_if_present(facts, "adapter_session_id", params.thread.sessionId)
  end
  if type(params.turn) == "table" then
    add_if_present(facts, "turn", params.turn)
    add_if_present(facts, "turn_id", params.turn.id)
    add_if_present(facts, "turn_status", params.turn.status)
  end
  if type(params.item) == "table" then
    add_if_present(facts, "item", params.item)
    add_if_present(facts, "item_id", params.item.id)
    add_if_present(facts, "item_type", params.item.type)
  end
  if type(params.hook) == "table" then
    add_if_present(facts, "hook", params.hook)
  end
  return facts
end

local function response_facts(message, context)
  local facts = {}
  add_if_present(facts, "rpc_id", message.id)
  if context.request_method ~= nil then
    facts.method = context.request_method
  end
  add_if_present(facts, "result", message.result)
  add_if_present(facts, "error", message.error)
  if type(message.result) == "table" then
    local result = message.result
    if type(result.thread) == "table" then
      add_if_present(facts, "thread", result.thread)
      add_if_present(facts, "thread_id", result.thread.id)
      add_if_present(facts, "adapter_session_id", result.thread.sessionId)
    end
    if type(result.turn) == "table" then
      add_if_present(facts, "turn", result.turn)
      add_if_present(facts, "turn_id", result.turn.id)
      add_if_present(facts, "turn_status", result.turn.status)
    end
    if type(result.item) == "table" then
      add_if_present(facts, "item", result.item)
      add_if_present(facts, "item_id", result.item.id)
    end
  end
  return facts
end

local function response_native_id(message)
  if type(message.result) == "table" then
    if type(message.result.thread) == "table" and is_id(message.result.thread.id) then
      return message.result.thread.id
    end
    if type(message.result.turn) == "table" and is_id(message.result.turn.id) then
      return message.result.turn.id
    end
    if type(message.result.item) == "table" and is_id(message.result.item.id) then
      return message.result.item.id
    end
  end
  return message.id
end

local function notification_native_id(method, params, message)
  if approval_methods[method] then
    return message.id or params.requestId or params.approvalId
  end
  if method == "thread/started" and type(params.thread) == "table" then
    return params.thread.id
  end
  if method == "turn/started" or method == "turn/completed" then
    if type(params.turn) == "table" then
      return params.turn.id
    end
    return params.turnId
  end
  if method == "item/started" or method == "item/completed" then
    if type(params.item) == "table" then
      return params.item.id
    end
    return params.itemId
  end
  if method == "turn/plan/updated" then
    return params.turnId
  end
  if method == "serverRequest/resolved" then
    return params.requestId or params.id
  end
  if method == "thread/compacted" then
    return params.turnId or params.threadId
  end
  if string.sub(method, 1, 5) == "item/" then
    return params.itemId or (type(params.item) == "table" and params.item.id) or params.turnId or params.threadId
  end
  return params.threadId or params.turnId or params.itemId or params.requestId or params.approvalId or params.id or message.id
end

local function notification_session_id(method, params, facts, metadata)
  if metadata.adapter_session_id ~= nil then
    return metadata.adapter_session_id
  end
  if metadata.thread_id ~= nil then
    return metadata.thread_id
  end
  if facts.adapter_session_id ~= nil then
    return facts.adapter_session_id
  end
  if params.threadId ~= nil then
    return params.threadId
  end
  if type(params.thread) == "table" and params.thread.sessionId ~= nil then
    return params.thread.sessionId
  end
  return nil
end

local function parse_app_server(value, context)
  local message, decode_error = decode(value)
  if message == nil then
    return decode_error
  end
  local metadata, context_error = check_context(context, message)
  if metadata == nil then
    return context_error
  end
  if type(context) ~= "table" then
    context = {}
  end
  local method = message.method
  local message_id = message.id
  local has_method = method ~= nil
  local has_result = message.result ~= nil
  local has_error = message.error ~= nil
  if has_method and type(method) ~= "string" then
    return diagnostic("malformed_event", "app-server method must be a string", message, {
      field = "method",
    })
  end
  if message.params ~= nil and type(message.params) ~= "table" then
    return diagnostic("malformed_event", "app-server params must be an object", message, {
      field = "params",
    })
  end
  if message_id ~= nil and not is_id(message_id) then
    return diagnostic("malformed_event", "app-server id must be an opaque string or number", message, {
      field = "id",
    })
  end
  if has_method and (has_result or has_error) then
    return diagnostic("malformed_event", "app-server method message cannot contain result or error", message)
  end
  if not has_method and not has_result and not has_error then
    return diagnostic("malformed_event", "app-server message must contain method, result, or error", message)
  end
  if not has_method and has_result and has_error then
    return diagnostic("malformed_event", "app-server response cannot contain both result and error", message)
  end
  if not has_method then
    if not is_id(message_id) then
      return diagnostic("malformed_event", "app-server response requires an opaque id", message, {
        field = "id",
      })
    end
    local facts = response_facts(message, context)
    local native_id = response_native_id(message)
    local adapter_session_id = metadata.adapter_session_id or facts.adapter_session_id or context.thread_id
    if native_id == nil then
      return diagnostic("malformed_event", "app-server response has no authoritative native id", message)
    end
    local kind = "managed_response"
    if facts.method == "initialize" then
      kind = "managed_initialized"
    elseif facts.method == "turn/interrupt" then
      kind = "managed_interruption_completed"
    end
    if message.error ~= nil then
      kind = "managed_response_error"
    end
    return envelope(kind, native_id, adapter_session_id, facts, message, metadata)
  end
  local params = message.params or {}
  if method == "initialized" then
    return {
      protocol = "initialized",
      method = method,
      facts = {},
      raw = copy(message),
    }
  end
  if method == "initialize" then
    if not is_id(message_id) then
      return diagnostic("malformed_event", "initialize requires an opaque request id", message, {
        field = "id",
      })
    end
    local validation = validate_client_params(method, params, message)
    if validation ~= true then
      return validation
    end
    return envelope("managed_initialize", message_id, metadata.adapter_session_id, {
      method = method,
      rpc_id = message_id,
      params = copy(params),
    }, message, metadata)
  end
  if client_methods[method] and message_id ~= nil then
    if not is_id(message_id) then
      return diagnostic("malformed_event", "app-server request id must be an opaque string or number", message, {
        field = "id",
      })
    end
    local validation = validate_client_params(method, params, message)
    if validation ~= true then
      return validation
    end
    return envelope("managed_request", message_id, metadata.adapter_session_id or context.thread_id, {
      method = method,
      rpc_id = message_id,
      params = copy(params),
    }, message, metadata)
  end
  local kind = method_kinds[method]
  if kind == nil then
    if message_id ~= nil and not is_id(message_id) then
      return diagnostic("malformed_event", "app-server request id must be an opaque string or number", message, {
        field = "id",
      })
    end
    local unknown_id = notification_native_id(method, params, message)
    if unknown_id == nil then
      return {
        protocol = "unknown_notification",
        method = method,
        facts = {},
        raw = copy(message),
      }
    end
    local unknown_facts = facts_from_params(method, params, message_id)
    unknown_facts.unknown_method = true
    return envelope("managed_notification", unknown_id, notification_session_id(method, params, unknown_facts, metadata), unknown_facts, message, metadata)
  end
  local validation = validate_notification_params(method, params, message)
  if validation ~= true then
    return validation
  end
  local facts = facts_from_params(method, params, message_id)
  local native_id = notification_native_id(method, params, message)
  if native_id == nil then
    return diagnostic("malformed_event", "app-server notification has no authoritative native id", message, {
      method = method,
    })
  end
  if method == "turn/completed" and type(params.turn) == "table" and params.turn.status == "interrupted" then
    kind = "managed_turn_interrupted"
  elseif method == "item/started" and type(params.item) == "table" and params.item.type == "contextCompaction" then
    kind = "managed_compaction_started"
  elseif method == "item/completed" and type(params.item) == "table" and params.item.type == "contextCompaction" then
    kind = "managed_compaction_completed"
  end
  local adapter_session_id = notification_session_id(method, params, facts, metadata)
  return envelope(kind, native_id, adapter_session_id, facts, message, metadata)
end

adapter.parse_existing = parse_hook
adapter.parse_managed = parse_app_server
adapter.parse_app_server = parse_app_server
adapter.forward_app_server = parse_app_server

function adapter.parse(value, mode, context)
  if mode == nil and type(value) == "string" then
    local decoded, decode_error = decode(value)
    if decoded == nil then
      return decode_error
    end
    value = decoded
  end
  if mode == "hook" or mode == "hooks" or mode == "existing" or mode == "interactive" then
    return parse_hook(value, context)
  end
  if mode == "app_server" or mode == "app-server" or mode == "managed" or mode == "stream" or mode == "stream-json" then
    return parse_app_server(value, context)
  end
  if type(value) == "table" and value.hook_event_name ~= nil then
    return parse_hook(value, context)
  end
  if type(value) == "table" and (value.method ~= nil or value.result ~= nil or value.error ~= nil or value.id ~= nil) then
    return parse_app_server(value, context)
  end
  return diagnostic("malformed_event", "cannot determine Codex event source; specify hook or app_server mode", value)
end

local function normalized_success(result)
  local wrapped = {
    ok = true,
    events = {},
    diagnostics = {},
    event = result,
  }
  if result ~= nil and result.protocol ~= nil then
    wrapped.protocol = result
    wrapped.event = nil
  elseif result ~= nil then
    wrapped.events[1] = result
    for key, value in pairs(result) do
      wrapped[key] = copy(value)
    end
  end
  return wrapped
end

local function normalized_failure(result)
  return {
    ok = false,
    events = {},
    diagnostics = { result },
    diagnostic = result,
    error = result,
  }
end

adapter.normalize = function(value, mode, context)
  local result = adapter.parse(value, mode, context)
  if result ~= nil and result.ok == false then
    return normalized_failure(result)
  end
  return normalized_success(result)
end

adapter.parse_event = adapter.parse
adapter.read = adapter.parse

adapter.forward = function(value, mode_or_sink, context)
  if type(mode_or_sink) == "function" then
    local source_mode = context
    local event_context = nil
    if type(context) == "table" then
      source_mode = context.mode or context.source
      event_context = context
    end
    local event = adapter.parse(value, source_mode, event_context)
    if event ~= nil and event.ok == false then
      return { accepted = false, diagnostic = event }
    end
    local ok, sink_result = pcall(mode_or_sink, event)
    if not ok then
      return {
        accepted = false,
        event = event,
        diagnostic = diagnostic("forward_failed", "Codex event sink raised an error", event, {
          error = sink_result,
        }),
      }
    end
    if sink_result == false then
      return {
        accepted = false,
        event = event,
        diagnostic = diagnostic("forward_rejected", "Codex event sink returned false", event),
      }
    end
    return { accepted = true, event = event }
  end
  local mode = mode_or_sink
  if type(mode) == "table" and context == nil then
    context = mode
    mode = context.mode or context.source
  end
  local event = adapter.parse(value, mode, context)
  if event ~= nil and event.ok == false then
    return { accepted = false, diagnostic = event }
  end
  return { accepted = true, event = event }
end

adapter.forward_event = adapter.forward
adapter.ingest = adapter.forward

function adapter.make_forwarder(sink, mode, context)
  return function(value)
    local event = adapter.parse(value, mode, context)
    if event ~= nil and event.ok == false then
      return { accepted = false, diagnostic = event }
    end
    if sink == nil then
      return { accepted = true, event = event }
    end
    local ok, sink_result = pcall(sink, event)
    if not ok then
      return {
        accepted = false,
        event = event,
        diagnostic = diagnostic("forward_failed", "Codex event sink raised an error", event, {
          error = sink_result,
        }),
      }
    end
    if sink_result == false then
      return {
        accepted = false,
        event = event,
        diagnostic = diagnostic("forward_rejected", "Codex event sink returned false", event),
      }
    end
    return { accepted = true, event = event }
  end
end


local control_methods = {
  initialize = true,
  ["thread/start"] = true,
  ["thread/resume"] = true,
  ["thread/fork"] = true,
  ["thread/unsubscribe"] = true,
  ["thread/archive"] = true,
  ["thread/delete"] = true,
  ["thread/unarchive"] = true,
  ["thread/settings/update"] = true,
  ["thread/inject_items"] = true,
  ["thread/compact/start"] = true,
  ["thread/shellCommand"] = true,
  ["turn/start"] = true,
  ["turn/steer"] = true,
  ["turn/interrupt"] = true,
}

function adapter.request(method, request_id, params)
  if not is_string(method) or not control_methods[method] then
    return diagnostic("unsupported_control", "Codex app-server control method is not declared", {
      method = method,
      id = request_id,
      params = params,
    }, {
      method = method,
    })
  end
  if not is_id(request_id) then
    return diagnostic("malformed_control", "control request requires an opaque id", {
      method = method,
      id = request_id,
      params = params,
    }, {
      field = "id",
    })
  end
  if params == nil then
    params = {}
  end
  if type(params) ~= "table" then
    return diagnostic("malformed_control", "control params must be an object", {
      method = method,
      id = request_id,
      params = params,
    }, {
      field = "params",
    })
  end
  local validation = validate_client_params(method, params, {
    method = method,
    id = request_id,
    params = params,
  })
  if validation ~= true then
    return validation
  end
  return {
    id = request_id,
    method = method,
    params = copy(params),
  }
end

adapter.control = adapter.request

function adapter.approval_response(request_id, result)
  if not is_id(request_id) then
    return diagnostic("malformed_control", "approval response requires an opaque request id", {
      id = request_id,
      result = result,
    }, {
      field = "id",
    })
  end
  if type(result) ~= "table" then
    return diagnostic("malformed_control", "approval response result must be an object", {
      id = request_id,
      result = result,
    }, {
      field = "result",
    })
  end
  return {
    id = request_id,
    result = copy(result),
  }
end

function adapter.handshake(request_id, client_info, capabilities)
  if not is_id(request_id) then
    return diagnostic("malformed_control", "initialize requires an opaque request id", {
      id = request_id,
      clientInfo = client_info,
      capabilities = capabilities,
    }, {
      field = "id",
    })
  end
  if type(client_info) ~= "table" or not is_string(client_info.name) then
    return diagnostic("malformed_control", "initialize requires clientInfo.name", {
      id = request_id,
      clientInfo = client_info,
      capabilities = capabilities,
    }, {
      field = "clientInfo",
    })
  end
  local params = {
    clientInfo = copy(client_info),
  }
  if capabilities ~= nil then
    if type(capabilities) ~= "table" then
      return diagnostic("malformed_control", "initialize capabilities must be an object", {
        id = request_id,
        capabilities = capabilities,
      }, {
        field = "capabilities",
      })
    end
    params.capabilities = copy(capabilities)
  end
  return {
    id = request_id,
    method = "initialize",
    params = params,
  }
end

adapter.hook_events = {
  "SessionStart",
  "SessionEnd",
  "UserPromptSubmit",
  "PreToolUse",
  "PermissionRequest",
  "PostToolUse",
  "PreCompact",
  "PostCompact",
  "SubagentStart",
  "SubagentStop",
  "Stop",
}

adapter.managed_notifications = {
  "thread/started",
  "thread/status/changed",
  "thread/closed",
  "thread/tokenUsage/updated",
  "turn/started",
  "turn/completed",
  "turn/plan/updated",
  "item/started",
  "item/completed",
  "item/agentMessage/delta",
  "item/plan/delta",
  "item/reasoning/summaryTextDelta",
  "item/reasoning/summaryPartAdded",
  "item/commandExecution/outputDelta",
  "item/commandExecution/terminalInteraction",
  "item/fileChange/outputDelta",
  "item/mcpToolCall/progress",
  "hook/started",
  "hook/completed",
  "thread/compacted",
  "serverRequest/resolved",
}

adapter.managed_methods = {
  "initialize",
  "initialized",
  "thread/start",
  "thread/resume",
  "thread/fork",
  "thread/unsubscribe",
  "thread/archive",
  "thread/delete",
  "thread/unarchive",
  "thread/settings/update",
  "thread/inject_items",
  "thread/compact/start",
  "thread/shellCommand",
  "turn/start",
  "turn/steer",
  "turn/interrupt",
  "item/commandExecution/requestApproval",
  "item/fileChange/requestApproval",
  "item/permissions/requestApproval",
}
adapter.managed_control_methods = adapter.managed_methods
adapter.managed_requests = {
  "item/commandExecution/requestApproval",
  "item/fileChange/requestApproval",
  "item/permissions/requestApproval",
  "serverRequest/resolved",
}

function adapter.start(options)
  if options ~= nil and type(options) ~= "table" then
    return { supported = false, diagnostic = diagnostic("malformed_control", "Codex start options must be an object") }
  end
  return {
    supported = true,
    executable = "codex",
    arguments = { "app-server" },
    transport = "stdio_jsonl",
    framing = "lf_jsonl",
  }
end

local function control_id(options)
  options = options or {}
  return options.request_id or options.native_id or options.id
end

local function control_thread(options)
  options = options or {}
  return options.thread_id or options.adapter_session_id or options.native_id
end

function adapter.prompt(message, options)
  if not is_string(message) then return diagnostic("malformed_control", "Codex prompt must be a non-empty string") end
  options = options or {}
  local id = control_id(options)
  local thread = control_thread(options)
  if not is_id(id) or not is_id(thread) then
    return diagnostic("malformed_control", "Codex prompt requires opaque request and thread IDs")
  end
  return adapter.request("turn/start", id, {
    threadId = thread,
    input = { { type = "text", text = message } },
  })
end
adapter.send_text = adapter.prompt
function adapter.abort(options)
  options = options or {}
  local id = control_id(options)
  local thread = control_thread(options)
  if not is_id(id) or not is_id(thread) then
    return diagnostic("malformed_control", "Codex abort requires opaque request and thread IDs")
  end
  return adapter.request("turn/interrupt", id, { threadId = thread })
end
adapter.pause = function() return diagnostic("unsupported_control", "Codex pause is unavailable") end
adapter.resume = function() return diagnostic("unsupported_control", "Codex resume is unavailable") end
adapter.retry = function() return diagnostic("unsupported_control", "Codex retry is unavailable") end
adapter.send_keys = function() return diagnostic("unsupported_control", "Codex keyboard control is unavailable") end

adapter.id = PROVIDER
adapter.adapter = PROVIDER
adapter.display_name = "OpenAI Codex CLI"
adapter.manifest_path = "manifest.json"
adapter.protocol = {
  existing = {
    kind = "hooks",
    transport = "command_json_stdin",
    controls = { prompt = "unavailable", steer = "unavailable", follow_up = "unavailable", abort = "unavailable" },
  },
  managed = {
    kind = "app_server",
    executable = "codex",
    arguments = { "app-server" },
    transport = "stdio_jsonl",
    controls = { prompt = "turn/start", steer = "turn/steer", follow_up = "turn/steer", abort = "turn/interrupt" },
  },
}
adapter.capabilities = {
  existing = { "session", "prompt_observation", "tool_observation", "permission_observation", "compaction_observation", "subagent_observation", "stop_observation", "end_observation" },
  managed = { "thread", "turn", "item", "approval", "plan", "compaction", "interruption", "token_usage", "prompt", "steer", "follow_up", "abort" },
}
adapter.fidelity_limits = {
  "native IDs remain opaque and are preserved without reformatting",
  "facts are copied only when present in authoritative hook or app-server payloads",
  "terminal output, screen state, process names, and transcript tails are never facts",
  "hook order, completion, cost, rate limits, and terminal association are never inferred",
}
adapter.manifest = {
  id = PROVIDER,
  provider = PROVIDER,
  version = VERSION,
  source_revision = SOURCE_REVISION,
  entrypoint = "adapter.luau",
  modes = { "hooks", "managed" },
  authority = { "hooks", "app-server" },
}

return adapter
